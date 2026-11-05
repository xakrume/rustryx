import { configure, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'
import { expectNoA11yViolations, renderApp } from '@/test/utils'

// The whole suite runs in parallel workers: the first render of a page loads a route chunk and
// can take longer than the default one second on a busy machine.
configure({ asyncUtilTimeout: 5000 })

const envelope = (code: string, message: string, status: number) =>
  HttpResponse.json(
    { error: { code, message, request_id: `req-${code}` } },
    { status, headers: { 'X-Request-Id': `req-${code}` } },
  )

const scenario = (name: string) => window.sessionStorage.setItem('ryx-mock-status', name)

beforeEach(() => window.sessionStorage.removeItem('ryx-mock-status'))

async function readiness() {
  const card = (await screen.findByRole('heading', { name: 'Readiness of this node' })).closest(
    'div.rounded-xl',
  ) as HTMLElement
  return within(card)
}

describe('system status page', () => {
  it('shows a healthy system quietly: ready, dependencies reachable, checks collapsed', async () => {
    const { container } = renderApp('/admin/system/status', { user: 'admin' })
    expect(await screen.findByText('All checks passed')).toBeInTheDocument()
    expect((await readiness()).getByText('Ready')).toBeInTheDocument()
    const deps = screen.getByRole('list', { name: 'Dependency health' })
    expect(within(deps).getAllByText('Reachable')).toHaveLength(5)
    expect(screen.getByText(/All 7 checks are healthy/)).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('shows what needs attention first, with the fix hint and a link', async () => {
    scenario('mixed')
    renderApp('/admin/system/status', { user: 'admin' })
    expect(await screen.findByText('Some checks need attention')).toBeInTheDocument()
    const checksHeading = screen.getByRole('heading', { name: 'Checks' })
    const list = checksHeading.closest('div.rounded-xl')!.querySelector('ul')!
    const first = within(list).getAllByRole('listitem')[0]!
    // Worst first: a warning before the healthy ones.
    expect(within(first).getAllByText('Warning').length).toBeGreaterThan(0)
    expect(screen.getByText(/How to fix: Raise the quota or move content/)).toBeInTheDocument()
    expect(screen.getAllByRole('link', { name: /^Open / }).length).toBeGreaterThan(0)
  })

  it('never shows an unknown check as green', async () => {
    scenario('mixed')
    renderApp('/admin/system/status', { user: 'admin' })
    const deps = await screen.findByRole('list', { name: 'Dependency health' })
    const ldap = within(deps).getByText('LDAP').closest('li')!
    expect(within(ldap).getByText('Unknown')).toBeInTheDocument()
    expect(within(ldap).queryByText('Reachable')).not.toBeInTheDocument()
  })

  it('must_not_change_the_readiness_wording_when_dependencies_are_down', async () => {
    scenario('dependencies-down')
    renderApp('/admin/system/status', { user: 'admin' })
    const deps = await screen.findByRole('list', { name: 'Dependency health' })
    expect(within(deps).getAllByText('Unreachable').length).toBeGreaterThanOrEqual(3)
    const card = await readiness()
    // Redis, Keycloak and LDAP are down; the node is still ready, and the page says so.
    expect(card.getByText('Ready')).toBeInTheDocument()
    expect(screen.queryByText('Not ready')).not.toBeInTheDocument()
    expect(screen.getByText(/never changes the readiness of this node/)).toBeInTheDocument()
    expect(
      card.getByText(/Advisories are reported by the readiness probe and never change readiness/),
    ).toBeInTheDocument()
  })

  it('must_say_not_ready_only_when_the_readiness_probe_says_so', async () => {
    scenario('not-ready')
    renderApp('/admin/system/status', { user: 'admin' })
    const card = await readiness()
    expect(card.getByText('Not ready')).toBeInTheDocument()
    expect(card.getByText('Fail')).toBeInTheDocument()
    expect(card.getByText('PostgreSQL answers')).toBeInTheDocument()
  })

  it('must_not_derive_readiness_from_the_overall_status', async () => {
    renderApp('/admin/system/status', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/system/status', () =>
          HttpResponse.json({
            status: 'error',
            generated_at: '2026-10-24T09:15:30Z',
            node: 'rustryx-0',
            checks: [
              {
                id: 'oidc_reachable',
                title: 'Keycloak',
                status: 'error',
                severity: 'critical',
                message: 'down',
                fix_hint: null,
                since: null,
                link: null,
              },
            ],
            readiness: {
              status: 'ready',
              checks: [{ name: 'database', status: 'ok' }],
              advisories: [],
            },
          }),
        ),
      ],
    })
    expect(await screen.findByText('Some checks failed')).toBeInTheDocument()
    expect((await readiness()).getByText('Ready')).toBeInTheDocument()
  })

  it('says when this server does not report a dependency', async () => {
    renderApp('/admin/system/status', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/system/status', () =>
          HttpResponse.json({
            status: 'ok',
            generated_at: '2026-10-24T09:15:30Z',
            node: 'rustryx-0',
            checks: [],
            readiness: { status: 'ready', checks: [], advisories: [] },
          }),
        ),
      ],
    })
    const deps = await screen.findByRole('list', { name: 'Dependency health' })
    expect(within(deps).getAllByText('This server does not report this dependency.')).toHaveLength(
      5,
    )
    expect(screen.getByText('This node reports no checks.')).toBeInTheDocument()
  })

  it('lists the nodes with version, uptime, leadership and the schema version', async () => {
    scenario('mixed')
    renderApp('/admin/system/status', { user: 'admin' })
    const table = await screen.findByRole('region', { name: 'Nodes' })
    expect(await within(table).findByText('rustryx-1')).toBeInTheDocument()
    expect(within(table).getByText('This node')).toBeInTheDocument()
    expect(within(table).getAllByText('0.1.0')).toHaveLength(2)
    expect(within(table).getByText('4 d 1 h')).toBeInTheDocument()
    expect(within(table).getByText('blob-gc, cleanup')).toBeInTheDocument()
    // The second node has not reported for five minutes.
    expect(within(table).getByText('Heartbeat is stale')).toBeInTheDocument()
    expect(screen.getByText(/Database schema version 42/)).toBeInTheDocument()
    expect(screen.getByText('2 nodes', { exact: false })).toBeInTheDocument()
  })

  it('explains that a single node setup is fine', async () => {
    renderApp('/admin/system/status', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/system/nodes', () =>
          HttpResponse.json({
            items: [
              {
                id: 'rustryx-0',
                version: '0.1.0',
                started_at: '2026-10-20T07:02:11Z',
                last_heartbeat_at: new Date().toISOString(),
                draining: true,
                leader_of: [],
              },
            ],
            next: null,
          }),
        ),
      ],
    })
    expect(await screen.findByText(/One node is running/)).toBeInTheDocument()
    expect(await screen.findByText('Draining')).toBeInTheDocument()
  })

  it('keeps the rest of the page when only the node list fails, and shows its code', async () => {
    renderApp('/admin/system/status', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/system/nodes', () => envelope('dependency_unavailable', 'Down', 503)),
      ],
    })
    expect(await screen.findByText('The node list could not be loaded')).toBeInTheDocument()
    expect(screen.getByText('req-dependency_unavailable')).toBeInTheDocument()
    expect((await readiness()).getByText('Ready')).toBeInTheDocument()
  })

  it('shows the live counters and job health of this node', async () => {
    scenario('mixed')
    renderApp('/admin/system/status', { user: 'admin' })
    expect(await screen.findByText('Requests and errors')).toBeInTheDocument()
    expect(await screen.findByText('Server errors (5xx)')).toBeInTheDocument()
    expect(screen.getByText('Job health')).toBeInTheDocument()
    expect(await screen.findByText('blob-gc')).toBeInTheDocument()
    expect(screen.getByText('Stale')).toBeInTheDocument()
  })

  it('shows the error with code and request id when the status cannot be loaded', async () => {
    renderApp('/admin/system/status', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/system/status', () => envelope('store_unavailable', 'Down', 503)),
      ],
    })
    expect(await screen.findByText('The system status could not be loaded')).toBeInTheDocument()
    expect(screen.getByText('store_unavailable')).toBeInTheDocument()
    expect(screen.getByText('req-store_unavailable')).toBeInTheDocument()
  })

  it('refreshes on demand', async () => {
    const user = userEvent.setup()
    let calls = 0
    renderApp('/admin/system/status', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/system/status', () => {
          calls += 1
          return undefined
        }),
      ],
    })
    await screen.findByText('All checks passed')
    const before = calls
    await user.click(screen.getByRole('button', { name: 'Refresh' }))
    await screen.findByText('All checks passed')
    expect(calls).toBeGreaterThan(before)
  })

  it('must_show_the_forbidden_panel_to_a_non_administrator', async () => {
    renderApp('/admin/system/status', { user: 'alice' })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
    expect(
      screen.queryByRole('heading', { name: 'Readiness of this node' }),
    ).not.toBeInTheDocument()
  })
})
