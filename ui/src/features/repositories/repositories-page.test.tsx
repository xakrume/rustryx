import { configure, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it } from 'vitest'
import { resetContent } from '@/features/components/mock-content'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import { resetRepositories } from './mock-state'

// The first renders of a file are slow when many test files run at once.
configure({ asyncUtilTimeout: 10_000 })

beforeEach(() => {
  resetRepositories()
  resetContent()
})

async function listLoaded() {
  return screen.findByRole('link', { name: 'maven-releases' })
}

describe('repository list', () => {
  it('shows name, format, type, status, blob store, usage and URL of each repository', async () => {
    const { container } = renderApp('/repositories?q=maven-releases', { user: 'admin' })
    await listLoaded()
    const row = screen.getByRole('row', { name: /maven-releases/ })
    expect(within(row).getByText('Maven')).toBeInTheDocument()
    expect(within(row).getByText('Hosted')).toBeInTheDocument()
    expect(within(row).getByText('Online')).toBeInTheDocument()
    expect(within(row).getByText('default')).toBeInTheDocument()
    expect(
      within(row).getByText('https://repo.example.org/repository/maven-releases/'),
    ).toBeVisible()
    expect(
      within(row).getByRole('button', { name: /^Copy URL of maven-releases/ }),
    ).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('keeps the filters in the address bar and narrows the list server side', async () => {
    const user = userEvent.setup()
    const { router } = renderApp('/repositories', { user: 'admin' })
    await listLoaded()
    await user.selectOptions(screen.getByLabelText('Type'), 'proxy')
    await waitFor(() => expect(router.state.location.search).toMatchObject({ repo_type: 'proxy' }))
    await waitFor(() =>
      expect(screen.queryByRole('link', { name: 'maven-releases' })).not.toBeInTheDocument(),
    )
    expect(await screen.findByRole('link', { name: 'maven-proxy' })).toBeInTheDocument()
    expect(screen.getByRole('list', { name: 'Active filters' })).toHaveTextContent('Proxy')
  })

  it('shows the filtered empty state with a way back', async () => {
    const user = userEvent.setup()
    renderApp('/repositories?q=zzz-nothing-matches', { user: 'admin' })
    expect(await screen.findByText('No results for these filters')).toBeInTheDocument()
    await user.click(screen.getAllByRole('button', { name: 'Clear filters' })[0]!)
    expect(await listLoaded()).toBeInTheDocument()
  })

  it('pages with load more and keeps what was loaded', async () => {
    const user = userEvent.setup()
    renderApp('/repositories', { user: 'admin' })
    await listLoaded()
    expect(screen.getByText('50 rows')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Load more' }))
    expect(await screen.findByText('100 rows')).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'maven-releases' })).toBeInTheDocument()
  })

  it('offers an administrator the create button and a row menu with edit and delete', async () => {
    const user = userEvent.setup()
    renderApp('/repositories?q=maven-releases', { user: 'admin' })
    await listLoaded()
    expect(screen.getByRole('link', { name: 'Create repository' })).toHaveAttribute(
      'href',
      '/repositories/new',
    )
    await user.click(screen.getByRole('button', { name: 'Actions for maven-releases' }))
    expect(await screen.findByRole('menuitem', { name: 'Open' })).toBeInTheDocument()
    expect(screen.getByRole('menuitem', { name: 'Client setup' })).toBeInTheDocument()
    expect(screen.getByRole('menuitem', { name: 'Edit' })).toHaveAttribute(
      'href',
      '/repositories/maven-releases/settings',
    )
    expect(screen.getByRole('menuitem', { name: 'Delete' })).toBeInTheDocument()
  })

  it('must_not_offer_create_edit_or_delete_to_a_user_without_the_privilege', async () => {
    const user = userEvent.setup()
    renderApp('/repositories?q=maven-releases', { user: 'alice' })
    await listLoaded()
    expect(screen.queryByRole('link', { name: 'Create repository' })).not.toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Actions for maven-releases' }))
    expect(await screen.findByRole('menuitem', { name: 'Open' })).toBeInTheDocument()
    expect(screen.queryByRole('menuitem', { name: 'Edit' })).not.toBeInTheDocument()
    expect(screen.queryByRole('menuitem', { name: 'Delete' })).not.toBeInTheDocument()
  })

  it('shows an anonymous visitor only the public repositories', async () => {
    renderApp('/repositories?q=maven')
    expect(await screen.findByRole('link', { name: 'maven-proxy' })).toBeInTheDocument()
    expect(screen.queryByRole('link', { name: 'maven-releases' })).not.toBeInTheDocument()
  })

  it('must_not_delete_a_repository_without_a_confirmation_that_names_it', async () => {
    const user = userEvent.setup()
    renderApp('/repositories?q=team-005', { user: 'admin' })
    await screen.findByRole('link', { name: 'team-005-raw' })
    await user.click(screen.getByRole('button', { name: 'Actions for team-005-raw' }))
    await user.click(await screen.findByRole('menuitem', { name: 'Delete' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(dialog).toHaveTextContent('Delete repository team-005-raw?')
    const confirm = within(dialog).getByRole('button', { name: 'Delete' })
    expect(confirm).toBeDisabled()
    await user.type(within(dialog).getByRole('textbox'), 'team-005-ra')
    expect(confirm).toBeDisabled()
    await user.type(within(dialog).getByRole('textbox'), 'w')
    expect(confirm).toBeEnabled()
    await user.click(confirm)
    await waitFor(() =>
      expect(screen.queryByRole('link', { name: 'team-005-raw' })).not.toBeInTheDocument(),
    )
    expect(await screen.findByText('Repository team-005-raw deleted.')).toBeInTheDocument()
  })

  it('asks again before deleting a repository that still holds components', async () => {
    const user = userEvent.setup()
    renderApp('/repositories?q=team-002', { user: 'admin' })
    await screen.findByRole('link', { name: 'team-002-pypi' })
    await user.click(screen.getByRole('button', { name: 'Actions for team-002-pypi' }))
    await user.click(await screen.findByRole('menuitem', { name: 'Delete' }))
    const dialog = await screen.findByRole('alertdialog')
    await user.type(within(dialog).getByRole('textbox'), 'team-002-pypi')
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }))
    // The server refused: the same dialog now names the content and asks once more.
    expect(await within(dialog).findByText(/still holds \d+ components/)).toBeInTheDocument()
    // The page behind a modal dialog is hidden from assistive tech, so look for it explicitly.
    expect(screen.getByRole('link', { name: 'team-002-pypi', hidden: true })).toBeInTheDocument()
    await user.click(within(dialog).getByRole('button', { name: 'Delete repository and content' }))
    await waitFor(() =>
      expect(screen.queryByRole('link', { name: 'team-002-pypi' })).not.toBeInTheDocument(),
    )
  })

  it('lists the groups that still use a repository and deletes nothing', async () => {
    const user = userEvent.setup()
    renderApp('/repositories?q=maven-releases', { user: 'admin' })
    await listLoaded()
    await user.click(screen.getByRole('button', { name: 'Actions for maven-releases' }))
    await user.click(await screen.findByRole('menuitem', { name: 'Delete' }))
    const dialog = await screen.findByRole('alertdialog')
    await user.type(within(dialog).getByRole('textbox'), 'maven-releases')
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }))
    expect(await within(dialog).findByText('These objects depend on it')).toBeInTheDocument()
    expect(within(dialog).getByText('maven-group')).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'maven-releases', hidden: true })).toBeInTheDocument()
  })

  it('shows the error with its code and request id and waits out Retry-After', async () => {
    renderApp('/repositories?q=error503', { user: 'admin' })
    expect(await screen.findByText('The list could not be loaded')).toBeInTheDocument()
    expect(screen.getByText('store_unavailable')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Retry/ })).toBeDisabled()
    expect(screen.getAllByText(/^0192f6c4-/).length).toBeGreaterThan(0)
  })

  it('opens the client setup of a repository in a drawer', async () => {
    const user = userEvent.setup()
    renderApp('/repositories?q=npm-releases', { user: 'admin' })
    await screen.findByRole('link', { name: 'npm-releases' })
    await user.click(screen.getByRole('button', { name: 'Actions for npm-releases' }))
    await user.click(await screen.findByRole('menuitem', { name: 'Client setup' }))
    const drawer = await screen.findByRole('dialog', { name: 'Client setup for npm-releases' })
    expect(await within(drawer).findByText('Resolve through the repository')).toBeInTheDocument()
    expect(within(drawer).getByRole('tab', { name: 'npm' })).toBeInTheDocument()
  })
})
