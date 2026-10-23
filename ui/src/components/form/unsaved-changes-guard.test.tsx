import { QueryClientProvider } from '@tanstack/react-query'
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Link,
  RouterProvider,
} from '@tanstack/react-router'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it } from 'vitest'
import { createTestQueryClient } from '@/test/utils'
import { UnsavedChangesGuard } from './unsaved-changes-guard'

function setup(dirty: boolean) {
  const root = createRootRoute()
  const editor = createRoute({
    getParentRoute: () => root,
    path: '/',
    component: () => (
      <>
        <UnsavedChangesGuard when={dirty} />
        <h1>Editor</h1>
        {/* The test router has its own routes; the app's registered route types do not know /other. */}
        <Link to={'/other' as never}>Leave</Link>
      </>
    ),
  })
  const other = createRoute({
    getParentRoute: () => root,
    path: '/other',
    component: () => <h1>Other page</h1>,
  })
  const router = createRouter({
    routeTree: root.addChildren([editor, other]),
    history: createMemoryHistory({ initialEntries: ['/'] }),
  })
  render(
    <QueryClientProvider client={createTestQueryClient()}>
      <RouterProvider router={router} />
    </QueryClientProvider>,
  )
  return router
}

describe('UnsavedChangesGuard', () => {
  it('lets the user leave a clean form without a prompt', async () => {
    const user = userEvent.setup()
    setup(false)
    await user.click(await screen.findByRole('link', { name: 'Leave' }))
    expect(await screen.findByRole('heading', { name: 'Other page' })).toBeInTheDocument()
  })

  it('must_not_leave_a_dirty_form_without_asking', async () => {
    const user = userEvent.setup()
    setup(true)
    await user.click(await screen.findByRole('link', { name: 'Leave' }))
    expect(
      await screen.findByRole('alertdialog', { name: 'Discard unsaved changes?' }),
    ).toBeInTheDocument()
    expect(screen.queryByRole('heading', { name: 'Other page' })).not.toBeInTheDocument()
  })

  it('stays on the form when the user keeps editing', async () => {
    const user = userEvent.setup()
    setup(true)
    await user.click(await screen.findByRole('link', { name: 'Leave' }))
    await user.click(await screen.findByRole('button', { name: 'Keep editing' }))
    await waitFor(() => expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument())
    expect(screen.getByRole('heading', { name: 'Editor' })).toBeInTheDocument()
  })

  it('leaves after the user confirms discarding', async () => {
    const user = userEvent.setup()
    setup(true)
    await user.click(await screen.findByRole('link', { name: 'Leave' }))
    await user.click(await screen.findByRole('button', { name: 'Discard and leave' }))
    expect(await screen.findByRole('heading', { name: 'Other page' })).toBeInTheDocument()
  })
})
