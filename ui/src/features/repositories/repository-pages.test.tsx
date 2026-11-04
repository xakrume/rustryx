import { configure, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'
import { resetContent } from '@/features/components/mock-content'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import { repositories } from '@/mocks/store'
import { bumpVersion, replaceRepository, resetRepositories } from './mock-state'

// The mock list hides private repositories from an anonymous caller, and these helpers run before sign in.
const findVisible = (name: string) => repositories.find((r) => r.name === name)

// The first renders of a file are slow when many test files run at once.
configure({ asyncUtilTimeout: 10_000 })

beforeEach(() => {
  resetRepositories()
  resetContent()
})

describe('repository detail', () => {
  it('shows the facts, usage, recent components and the client setup of a repository', async () => {
    const { container } = renderApp('/repositories/maven-releases', { user: 'admin' })
    expect(
      await screen.findByRole('heading', { level: 1, name: 'maven-releases' }),
    ).toBeInTheDocument()
    const main = within(screen.getByRole('main'))
    expect(await main.findByRole('heading', { name: 'Summary' })).toBeInTheDocument()
    expect(main.getByText('Deploy once')).toBeInTheDocument()
    expect(main.getByRole('heading', { name: 'Usage' })).toBeInTheDocument()
    expect(await main.findByRole('heading', { name: 'Recent components' })).toBeInTheDocument()
    expect(await main.findAllByRole('link', { name: /^com\.example\./ })).not.toHaveLength(0)
    expect(await main.findByRole('region', { name: 'Client setup' })).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('offers the tabs the caller may use and marks the current one', async () => {
    renderApp('/repositories/maven-releases', { user: 'admin' })
    const nav = await screen.findByRole('navigation', { name: 'Repository sections' })
    expect(within(nav).getByRole('link', { name: 'Overview' })).toHaveAttribute(
      'aria-current',
      'page',
    )
    for (const name of ['Browse', 'Components', 'Settings', 'Activity']) {
      expect(within(nav).getByRole('link', { name })).toBeInTheDocument()
    }
  })

  it('must_not_show_settings_or_activity_tabs_to_someone_who_cannot_use_them', async () => {
    renderApp('/repositories/maven-releases', { user: 'alice' })
    const nav = await screen.findByRole('navigation', { name: 'Repository sections' })
    expect(within(nav).queryByRole('link', { name: 'Settings' })).not.toBeInTheDocument()
    expect(within(nav).queryByRole('link', { name: 'Activity' })).not.toBeInTheDocument()
    expect(screen.queryByRole('switch', { name: 'Online' })).not.toBeInTheDocument()
  })

  it('has no browse tab for docker, which has no path tree', async () => {
    renderApp('/repositories/docker-releases', { user: 'admin' })
    const nav = await screen.findByRole('navigation', { name: 'Repository sections' })
    expect(within(nav).queryByRole('link', { name: 'Browse' })).not.toBeInTheDocument()
    expect(within(nav).getByRole('link', { name: 'Images' })).toBeInTheDocument()
  })

  it('says "not found or hidden" for a repository that does not exist', async () => {
    renderApp('/repositories/no-such-repository', { user: 'admin' })
    expect(
      await screen.findByRole('heading', { name: 'Not found or hidden from you' }),
    ).toBeInTheDocument()
    expect(screen.getByText('repository_not_found')).toBeInTheDocument()
  })

  it('names the members of a group in order and warns about an offline one', async () => {
    const group = findVisible('maven-group')!
    const member = findVisible('maven-releases')!
    replaceRepository({ ...member, online: false })
    replaceRepository({ ...group, group: { members: ['maven-releases', 'maven-proxy'] } })
    renderApp('/repositories/maven-group', { user: 'admin' })
    const heading = await screen.findByRole('heading', { name: 'Members' })
    const card = heading.closest('div')!.parentElement!
    const items = within(card).getAllByRole('listitem')
    expect(items[0]).toHaveTextContent('maven-releases')
    expect(items[1]).toHaveTextContent('maven-proxy')
    expect(await within(card).findByText(/Offline: maven-releases/)).toBeInTheDocument()
  })

  it('shows the upstream of a proxy and a note that cached content is still served', async () => {
    const proxy = findVisible('npm-proxy')!
    replaceRepository({
      ...proxy,
      upstream: {
        state: 'auto_blocked',
        blocked_until: null,
        last_error_code: 'upstream_timeout',
        last_checked_at: proxy.created_at,
        circuit: 'open',
      },
    })
    renderApp('/repositories/npm-proxy', { user: 'admin' })
    expect(await screen.findByRole('heading', { name: 'Upstream' })).toBeInTheDocument()
    expect(screen.getByText('Upstream auto-blocked')).toBeInTheDocument()
    expect(screen.getByText('upstream_timeout')).toBeInTheDocument()
    expect(screen.getByText(/cached content is still served/)).toBeInTheDocument()
  })

  it('shows an offline repository with a banner and lets an administrator switch it on', async () => {
    const user = userEvent.setup()
    const repo = findVisible('npm-releases')!
    replaceRepository({ ...repo, online: false })
    renderApp('/repositories/npm-releases', { user: 'admin' })
    expect(await screen.findByText(/Requests for its content answer 503/)).toBeInTheDocument()
    const toggle = screen.getByRole('switch', { name: 'Online' })
    expect(toggle).not.toBeChecked()
    await user.click(toggle)
    expect(await screen.findByText('npm-releases is online.')).toBeInTheDocument()
    await waitFor(() =>
      expect(screen.queryByText(/Requests for its content answer 503/)).not.toBeInTheDocument(),
    )
    expect(findVisible('npm-releases')?.online).toBe(true)
  })

  it('opens the three way dialog when the online switch used a stale version', async () => {
    const user = userEvent.setup()
    renderApp('/repositories/npm-releases', { user: 'admin' })
    const toggle = await screen.findByRole('switch', { name: 'Online' })
    // Someone else changes the repository after this page read it.
    bumpVersion(findVisible('npm-releases')!)
    await user.click(toggle)
    const dialog = await screen.findByRole('alertdialog')
    expect(dialog).toHaveTextContent('Changed by someone else')
    expect(
      within(dialog).getByRole('button', { name: 'Overwrite with my changes' }),
    ).toBeInTheDocument()
    await user.click(within(dialog).getByRole('button', { name: 'Overwrite with my changes' }))
    await waitFor(() => expect(findVisible('npm-releases')?.online).toBe(false))
  })

  it('shows an upload shortcut only for a repository the caller may write to', async () => {
    renderApp('/repositories/npm-releases', { user: 'admin' })
    const main = within(await screen.findByRole('main'))
    expect(await main.findByRole('link', { name: 'Upload' })).toHaveAttribute(
      'href',
      '/upload?repository=npm-releases',
    )
  })
})

describe('repository components tab', () => {
  it('lists components and filters them by name in the address bar', async () => {
    const user = userEvent.setup()
    const { router } = renderApp('/repositories/maven-releases/components', { user: 'admin' })
    await screen.findAllByRole('link', { name: /^com\.example\./ })
    await user.type(screen.getByLabelText('Search'), 'api-core')
    await waitFor(() => expect(router.state.location.search).toMatchObject({ q: 'api-core' }))
    await waitFor(() => {
      for (const link of screen.getAllByRole('link', { name: /^com\.example\./ })) {
        expect(link).toHaveTextContent(/api-core/)
      }
    })
  })

  it('shows images and tags for a docker repository and links to the image page', async () => {
    renderApp('/repositories/docker-releases/components', { user: 'admin' })
    const link = (await screen.findAllByRole('link', { name: 'acme/api' }))[0]!
    expect(link).toHaveAttribute(
      'href',
      expect.stringContaining('/repositories/docker-releases/images/'),
    )
    expect(screen.getByRole('columnheader', { name: /^Tag$/ })).toBeInTheDocument()
    expect(screen.getByRole('columnheader', { name: 'Tag count' })).toBeInTheDocument()
  })

  it('shows a designed empty state for a repository without components', async () => {
    renderApp('/repositories/team-005-raw/components', { user: 'admin' })
    expect(await screen.findByText('No components here')).toBeInTheDocument()
    const main = within(screen.getByRole('main'))
    expect(main.getAllByRole('link', { name: 'Upload' }).at(-1)).toHaveAttribute(
      'href',
      '/upload?repository=team-005-raw',
    )
  })
})

describe('image page', () => {
  it('lists the tags of an image with digest and pull command, and deletes one after confirmation', async () => {
    const user = userEvent.setup()
    renderApp('/repositories/docker-releases/images/acme%2Fapi', { user: 'admin' })
    expect(await screen.findByRole('heading', { name: 'acme/api' })).toBeInTheDocument()
    await screen.findAllByRole('button', { name: /^Copy pull command of acme\/api:/ })
    expect(screen.getAllByText(/^sha256:/).length).toBeGreaterThan(0)
    const remove = screen.getAllByRole('button', { name: /^Delete acme\/api:/ })[0]!
    await user.click(remove)
    const dialog = await screen.findByRole('alertdialog')
    expect(dialog).toHaveTextContent('Delete component acme/api:')
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }))
    expect(await screen.findByText(/^Deleted acme\/api:/)).toBeInTheDocument()
  })

  it('filters tags from the address bar and shows how to use the client', async () => {
    const user = userEvent.setup()
    const { router } = renderApp('/repositories/docker-releases/images/acme%2Fapi', {
      user: 'admin',
    })
    expect(await screen.findByRole('heading', { name: 'Client setup' })).toBeInTheDocument()
    await user.type(screen.getByLabelText('Tag contains'), 'latest')
    await waitFor(() => expect(router.state.location.search).toMatchObject({ q: 'latest' }))
    const table = within(await screen.findByRole('region', { name: 'Tags of acme/api' }))
    await waitFor(() => {
      const tags = table.getAllByRole('link')
      expect(tags.length).toBeGreaterThan(0)
      for (const tag of tags) expect(tag).toHaveTextContent('latest')
    })
  })

  it('says so when an image has no tags', async () => {
    renderApp('/repositories/docker-releases/images/nothing%2Fhere', { user: 'admin' })
    expect(await screen.findByText('No tags for this image')).toBeInTheDocument()
  })
})

describe('repository activity', () => {
  it('lists the audit events of the repository with a copyable request id', async () => {
    renderApp('/repositories/maven-releases/activity', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/audit-events', ({ request }) => {
          expect(new URL(request.url).searchParams.get('repository')).toBe('maven-releases')
          return HttpResponse.json({
            items: [
              {
                id: '0192f6c4-5a1e-7000-8000-0000000000a1',
                at: '2026-10-24T09:15:30Z',
                kind: 'content_change',
                actor_id: null,
                actor_name: 'alice',
                client_ip: null,
                request_id: '0192f6c4-5a1e-7000-8000-00000000c0de',
                action: 'asset.publish',
                target: 'maven-releases/com/example/lib/1.0/lib-1.0.jar',
                outcome: 'success',
                detail: {},
              },
            ],
            next: null,
          })
        }),
      ],
    })
    expect(await screen.findByText('asset.publish')).toBeInTheDocument()
    expect(screen.getByText('alice')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Copy Request ID' })).toBeInTheDocument()
  })

  it('shows the empty state when there is no activity', async () => {
    renderApp('/repositories/maven-releases/activity', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/audit-events', () => HttpResponse.json({ items: [], next: null })),
      ],
    })
    expect(await screen.findByText('No activity yet')).toBeInTheDocument()
  })

  it('must_not_open_the_activity_tab_for_someone_without_audit_access', async () => {
    renderApp('/repositories/maven-releases/activity', { user: 'alice' })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
    expect(screen.getByText(/Missing privilege/)).toBeInTheDocument()
  })
})
