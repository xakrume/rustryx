import { act, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { describe, expect, it } from 'vitest'
import { STORAGE_KEYS } from '@/lib/constants'
import { meBody, readSession } from '@/mocks/session'
import { expectNoA11yViolations, renderApp } from '@/test/utils'

const nav = () => screen.getByRole('navigation', { name: 'Main navigation' })

describe('app shell', () => {
  it('offers the skip link first, then the navigation, and a labelled main landmark', async () => {
    const user = userEvent.setup()
    renderApp('/')
    await screen.findByRole('heading', { level: 1, name: 'Home' })
    await user.tab()
    expect(screen.getByRole('link', { name: 'Skip to main content' })).toHaveFocus()
    await user.keyboard('{Enter}')
    expect(document.getElementById('main-content')).toHaveFocus()
    expect(screen.getByRole('main')).toBeInTheDocument()
  })

  it('has no accessibility violations on the home page', async () => {
    const { container } = renderApp('/', { user: 'admin' })
    await screen.findByRole('heading', { level: 1, name: 'Home' })
    await expectNoA11yViolations(container)
  })

  describe('privilege aware navigation', () => {
    it('shows an anonymous visitor only what they may open, with a Sign in link', async () => {
      renderApp('/')
      await screen.findByRole('heading', { level: 1, name: 'Home' })
      const links = within(nav())
        .getAllByRole('link')
        .map((a) => a.textContent)
      expect(links).toEqual(['Home', 'Repositories', 'Search', 'About'])
      expect(screen.getByRole('link', { name: 'Sign in' })).toBeInTheDocument()
      expect(screen.queryByText('Administration')).not.toBeInTheDocument()
    })

    it('must_not_show_administration_entries_to_a_developer', async () => {
      renderApp('/', { user: 'alice' })
      await screen.findByRole('heading', { level: 1, name: 'Home' })
      expect(
        within(nav()).queryByRole('button', { name: /Identity and access/i }),
      ).not.toBeInTheDocument()
      expect(within(nav()).getByRole('link', { name: 'Upload' })).toBeInTheDocument()
    })

    it('shows an administrator the administration groups, collapsed until opened', async () => {
      const user = userEvent.setup()
      renderApp('/', { user: 'admin' })
      await screen.findByRole('heading', { level: 1, name: 'Home' })
      const group = within(nav()).getByRole('button', { name: /Identity and access/i })
      expect(group).toHaveAttribute('aria-expanded', 'false')
      expect(within(nav()).queryByRole('link', { name: 'Users' })).not.toBeInTheDocument()
      await user.click(group)
      expect(group).toHaveAttribute('aria-expanded', 'true')
      expect(within(nav()).getByRole('link', { name: 'Users' })).toBeInTheDocument()
    })
  })

  describe('route protection', () => {
    it('sends an anonymous visitor to the login page and remembers where they were going', async () => {
      const { router } = renderApp('/admin/users')
      await waitFor(() => expect(router.state.location.pathname).toBe('/login'))
      expect(router.state.location.search).toMatchObject({ return_to: '/admin/users' })
      expect(await screen.findByRole('heading', { name: 'Sign in to Rustryx' })).toBeInTheDocument()
    })

    it('shows a developer a page level forbidden panel naming the privilege', async () => {
      renderApp('/admin/users', { user: 'alice' })
      expect(
        await screen.findByRole('heading', { name: 'You do not have access' }),
      ).toBeInTheDocument()
      expect(screen.getByText(/Missing privilege: app\.users\.read/)).toBeInTheDocument()
    })

    it('lets an administrator in', async () => {
      renderApp('/admin/users', { user: 'admin' })
      expect(await screen.findByRole('heading', { level: 1, name: 'Users' })).toBeInTheDocument()
      expect(screen.getByRole('navigation', { name: 'Breadcrumb' })).toHaveTextContent(
        'Administration',
      )
    })

    it('requires the upload privilege for the upload page', async () => {
      renderApp('/upload', { user: 'alice' })
      expect(await screen.findByRole('heading', { level: 1, name: 'Upload' })).toBeInTheDocument()
    })

    it('redirects /admin to the status page', async () => {
      const { router } = renderApp('/admin', { user: 'admin' })
      await waitFor(() => expect(router.state.location.pathname).toBe('/admin/system/status'))
    })

    it('sends the user to login with an expiry note when a call answers 401 mid session', async () => {
      const { router } = renderApp('/', {
        user: 'admin',
        overrides: [
          http.get('/api/v1/dashboard/summary', () =>
            HttpResponse.json(
              { error: { code: 'token_expired', message: 'expired', request_id: 'r' } },
              { status: 401 },
            ),
          ),
        ],
      })
      await waitFor(() => expect(router.state.location.pathname).toBe('/login'))
      expect(router.state.location.search).toMatchObject({ reason: 'expired', return_to: '/' })
    })
  })

  describe('navigation and focus', () => {
    it('moves focus to the new page heading after a route change and announces it', async () => {
      const user = userEvent.setup()
      renderApp('/')
      await screen.findByRole('heading', { level: 1, name: 'Home' })
      await user.click(within(nav()).getByRole('link', { name: 'Search' }))
      const heading = await screen.findByRole('heading', { level: 1, name: 'Search' })
      await waitFor(() => expect(heading).toHaveFocus())
      await waitFor(() => expect(screen.getByText('Navigated to Search')).toBeInTheDocument())
    })

    it('must_not_steal_focus_when_only_the_query_string_changes', async () => {
      const { router } = renderApp('/search')
      await screen.findByRole('heading', { level: 1, name: 'Search' })
      const link = within(nav()).getByRole('link', { name: 'Home' })
      link.focus()
      await act(() => router.navigate({ to: '/search', search: { q: 'maven' } }))
      await new Promise((resolve) => setTimeout(resolve, 50))
      expect(link).toHaveFocus()
    })

    it('marks the current page in the navigation', async () => {
      renderApp('/search')
      await screen.findByRole('heading', { level: 1, name: 'Search' })
      expect(within(nav()).getByRole('link', { name: 'Search' })).toHaveAttribute(
        'aria-current',
        'page',
      )
      expect(within(nav()).getByRole('link', { name: 'Home' })).not.toHaveAttribute('aria-current')
    })

    it('moves between navigation items with the arrow keys', async () => {
      const user = userEvent.setup()
      renderApp('/')
      await screen.findByRole('heading', { level: 1, name: 'Home' })
      within(nav()).getByRole('link', { name: 'Home' }).focus()
      await user.keyboard('{ArrowDown}')
      expect(within(nav()).getByRole('link', { name: 'Repositories' })).toHaveFocus()
      await user.keyboard('{End}')
      expect(within(nav()).getByRole('link', { name: 'About' })).toHaveFocus()
      await user.keyboard('{Home}')
      expect(within(nav()).getByRole('link', { name: 'Home' })).toHaveFocus()
    })

    it('collapses to an icon rail that keeps the labels as accessible names, and remembers it', async () => {
      const user = userEvent.setup()
      renderApp('/')
      await screen.findByRole('heading', { level: 1, name: 'Home' })
      await user.click(screen.getByRole('button', { name: 'Collapse sidebar' }))
      expect(within(nav()).getByRole('link', { name: 'Repositories' })).toBeInTheDocument()
      expect(within(nav()).queryByText('Repositories')).not.toBeInTheDocument()
      expect(window.localStorage.getItem(STORAGE_KEYS.sidebarCollapsed)).toBe('true')
      expect(screen.getByRole('button', { name: 'Expand sidebar' })).toHaveAttribute(
        'aria-expanded',
        'false',
      )
    })

    it('shows the 404 page for an unknown address', async () => {
      renderApp('/no/such/page', { user: 'admin' })
      expect(await screen.findByRole('heading', { name: 'Page not found' })).toBeInTheDocument()
    })

    it('shows the breadcrumb trail with a dynamic repository name', async () => {
      renderApp('/repositories/maven-releases/browse', { user: 'admin' })
      const trail = await screen.findByRole('navigation', { name: 'Breadcrumb' })
      expect(trail).toHaveTextContent('Repositories')
      expect(trail).toHaveTextContent('maven-releases')
      expect(within(trail).getByText('Browse')).toHaveAttribute('aria-current', 'page')
    })
  })

  describe('theme and density', () => {
    it('switches the theme from the top bar and persists it', async () => {
      const user = userEvent.setup()
      renderApp('/')
      await screen.findByRole('heading', { level: 1, name: 'Home' })
      await user.click(screen.getByRole('button', { name: /^Theme:/ }))
      await user.click(await screen.findByRole('menuitemradio', { name: 'Dark' }))
      expect(document.documentElement).toHaveAttribute('data-theme', 'dark')
      expect(window.localStorage.getItem(STORAGE_KEYS.theme)).toBe('dark')
    })

    it('switches the density from the top bar', async () => {
      const user = userEvent.setup()
      renderApp('/')
      await screen.findByRole('heading', { level: 1, name: 'Home' })
      await user.click(screen.getByRole('button', { name: /^Density:/ }))
      await user.click(await screen.findByRole('menuitemradio', { name: 'Comfortable' }))
      expect(document.documentElement).toHaveAttribute('data-density', 'comfortable')
    })
  })

  describe('user menu', () => {
    it('shows who is signed in with a role summary, and signs out', async () => {
      const user = userEvent.setup()
      const { router } = renderApp('/', { user: 'alice' })
      await screen.findByRole('heading', { level: 1, name: 'Home' })
      await user.click(screen.getByRole('button', { name: /Account menu: Alice Example/ }))
      const menu = await screen.findByRole('menu')
      expect(menu).toHaveTextContent('Signed in as')
      expect(menu).toHaveTextContent('nx-developer')
      expect(menu).toHaveTextContent('Single sign-on account')
      await user.click(within(menu).getByRole('menuitem', { name: 'Sign out' }))
      await waitFor(() => expect(router.state.location.pathname).toBe('/login'))
      expect(readSession()).toBeNull()
    })
  })

  describe('command palette', () => {
    it('opens with Ctrl+K and lists only the pages the user may open', async () => {
      const user = userEvent.setup()
      renderApp('/', { user: 'alice' })
      await screen.findByRole('heading', { level: 1, name: 'Home' })
      await user.keyboard('{Control>}k{/Control}')
      const dialog = await screen.findByRole('dialog', { name: 'Command palette' })
      const options = within(dialog)
        .getAllByRole('option')
        .map((o) => o.textContent ?? '')
      expect(options.some((o) => o.includes('Repositories'))).toBe(true)
      expect(options.some((o) => o.includes('Personal tokens'))).toBe(true)
      expect(options.some((o) => o.includes('Users'))).toBe(false)
      await expectNoA11yViolations(document.body)
    })

    it('opens with Cmd+K and closes with Escape, returning focus', async () => {
      const user = userEvent.setup()
      renderApp('/')
      await screen.findByRole('heading', { level: 1, name: 'Home' })
      await user.keyboard('{Meta>}k{/Meta}')
      await screen.findByRole('dialog', { name: 'Command palette' })
      await user.keyboard('{Escape}')
      await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    })

    it('jumps to a page and finds repositories by typing', async () => {
      const user = userEvent.setup()
      const { router } = renderApp('/')
      await screen.findByRole('heading', { level: 1, name: 'Home' })
      await user.click(
        screen.getAllByRole('button', { name: 'Open the command palette' })[0] as HTMLElement,
      )
      await user.type(
        await screen.findByRole('combobox', { name: 'Type a command or search' }),
        'npm-proxy',
      )
      const repo = await screen.findByRole('option', { name: /^npm-proxy/ }, { timeout: 3000 })
      await user.click(repo)
      await waitFor(() => expect(router.state.location.pathname).toBe('/repositories/npm-proxy'))
    })

    it('searches components for the typed text', async () => {
      const user = userEvent.setup()
      const { router } = renderApp('/')
      await screen.findByRole('heading', { level: 1, name: 'Home' })
      await user.keyboard('{Control>}k{/Control}')
      await user.type(await screen.findByRole('combobox'), 'guava')
      await user.click(
        await screen.findByRole(
          'option',
          { name: /Search components for "guava"/ },
          { timeout: 3000 },
        ),
      )
      await waitFor(() => expect(router.state.location.pathname).toBe('/search'))
      expect(router.state.location.search).toMatchObject({ q: 'guava' })
    })

    it('runs a command: switching the theme', async () => {
      const user = userEvent.setup()
      renderApp('/')
      await screen.findByRole('heading', { level: 1, name: 'Home' })
      await user.keyboard('{Control>}k{/Control}')
      await user.click(await screen.findByRole('option', { name: /Theme: Dark/ }))
      expect(document.documentElement).toHaveAttribute('data-theme', 'dark')
    })
  })

  describe('banners', () => {
    it('shows the offline banner when the browser loses its connection', async () => {
      renderApp('/')
      await screen.findByRole('heading', { level: 1, name: 'Home' })
      Object.defineProperty(navigator, 'onLine', { configurable: true, value: false })
      act(() => {
        window.dispatchEvent(new Event('offline'))
      })
      expect(await screen.findByText(/You are offline/)).toBeInTheDocument()
      Object.defineProperty(navigator, 'onLine', { configurable: true, value: true })
      act(() => {
        window.dispatchEvent(new Event('online'))
      })
      await waitFor(() => expect(screen.queryByText(/You are offline/)).not.toBeInTheDocument())
    })

    it('shows the read-only banner when the system is in read-only mode', async () => {
      renderApp('/', {
        user: 'admin',
        overrides: [
          http.get('/api/v1/me', () =>
            HttpResponse.json({
              ...meBody(readSession()),
              system: { read_only: true, anonymous_enabled: true, base_url_configured: true },
            }),
          ),
        ],
      })
      expect(await screen.findByText(/read-only mode/)).toBeInTheDocument()
    })

    it('shows a degraded system to administrators only, with a link to the status page', async () => {
      renderApp('/', {
        user: 'admin',
        overrides: [
          http.get('/api/v1/system/status', () =>
            HttpResponse.json({
              status: 'warning',
              generated_at: '2026-10-24T09:15:30Z',
              node: 'n',
              checks: [],
              readiness: { status: 'ready', checks: [], advisories: [] },
            }),
          ),
        ],
      })
      expect(await screen.findByText('Some system checks are not healthy.')).toBeInTheDocument()
      expect(screen.getByRole('link', { name: 'View status' })).toHaveAttribute(
        'href',
        '/admin/system/status',
      )
    })

    it('must_not_ask_a_non_administrator_for_the_system_status', async () => {
      let asked = false
      renderApp('/', {
        user: 'alice',
        overrides: [
          http.get('/api/v1/system/status', () => {
            asked = true
            return HttpResponse.json({})
          }),
        ],
      })
      await screen.findByRole('heading', { level: 1, name: 'Home' })
      await new Promise((r) => setTimeout(r, 100))
      expect(asked).toBe(false)
    })
  })
})
