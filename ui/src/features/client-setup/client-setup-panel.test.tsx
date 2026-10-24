import { configure, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'
import { resetContent } from '@/features/components/mock-content'
import { resetRepositories } from '@/features/repositories/mock-state'
import { expectNoA11yViolations, renderApp } from '@/test/utils'

// The first renders of a file are slow when many test files run at once.
configure({ asyncUtilTimeout: 10_000 })

beforeEach(() => {
  resetRepositories()
  resetContent()
})

async function panel(
  path = '/repositories/maven-releases',
  user: 'admin' | 'alice' | null = 'admin',
) {
  const result = renderApp(path, user ? { user } : {})
  const section = await screen.findByRole('region', { name: 'Client setup' })
  return { ...result, section: within(section), root: section }
}

const FAKE_TOKEN = 'ryx_TEST_FIXTURE_TOKEN_0123456789012345678901'

describe('client setup panel', () => {
  it('shows one tab per tool and the snippets of the active tab with marked placeholders', async () => {
    const { section, root } = await panel()
    expect(await section.findByRole('tab', { name: 'Maven' })).toHaveAttribute(
      'aria-selected',
      'true',
    )
    expect(section.getByText('Resolve through the repository')).toBeInTheDocument()
    expect(section.getByText('Credentials')).toBeInTheDocument()
    // Placeholders are marked in the text, and listed with their meaning.
    expect(root.querySelectorAll('[data-placeholder]').length).toBeGreaterThan(0)
    expect(section.getByText('<TOKEN>', { selector: 'dt' })).toBeInTheDocument()
    expect(
      section.getByRole('button', { name: /^Copy Resolve through the repository/ }),
    ).toBeInTheDocument()
    await expectNoA11yViolations(root)
  })

  it('offers publish snippets only to someone who may write', async () => {
    const admin = await panel('/repositories/npm-releases', 'admin')
    await admin.section.findByRole('tab', { name: 'npm' })
    expect(admin.section.getByText('Publish a package')).toBeInTheDocument()
    admin.unmount()

    const reader = await panel('/repositories/npm-proxy', 'alice')
    await reader.section.findByRole('tab', { name: 'npm' })
    expect(reader.section.queryByText('Publish a package')).not.toBeInTheDocument()
  })

  it('says that a public repository needs no credentials and leaves them out', async () => {
    // maven-proxy is public: an anonymous visitor sees it, and no snippet asks for a token.
    const { section, root } = await panel('/repositories/maven-proxy', null)
    expect(await section.findByText(/Anonymous read is on/)).toBeInTheDocument()
    expect(section.queryByText('Credentials')).not.toBeInTheDocument()
    expect(root.querySelectorAll('[data-placeholder]')).toHaveLength(0)
    expect(screen.queryByRole('button', { name: 'Create a token for me' })).not.toBeInTheDocument()
  })

  it('names the hosted member a group publishes to', async () => {
    const { section } = await panel('/repositories/maven-group')
    expect(
      await section.findByText(/Resolve through the group and publish to the hosted member/),
    ).toBeInTheDocument()
    expect(section.getByRole('link', { name: 'maven-releases' })).toHaveAttribute(
      'href',
      '/repositories/maven-releases',
    )
  })

  it('switches to the raw text without markers', async () => {
    const user = userEvent.setup()
    const { section, root } = await panel()
    await section.findByRole('tab', { name: 'Maven' })
    await user.click(section.getByRole('switch', { name: 'Show raw text' }))
    expect(root.querySelectorAll('[data-placeholder]')).toHaveLength(0)
    expect(root.textContent).toContain('<TOKEN>')
  })

  it('shows the private CA note and the missing base URL warning', async () => {
    const base = {
      repository: {
        id: '0192f6c4-5a1e-7000-8000-000000000101',
        name: 'maven-releases',
        format: 'maven2',
        repo_type: 'hosted',
      },
      base_url: 'http://localhost:3000',
      base_url_configured: false,
      variant: 'authenticated',
      publish_target: null,
      snippets: [],
      notes: [{ kind: 'warning', text: 'Mirror disabled by policy.' }],
      private_ca: true,
    }
    const { section } = await (async () => {
      renderApp('/repositories/maven-releases', {
        user: 'admin',
        overrides: [http.get('/api/v1/client-setup', () => HttpResponse.json(base))],
      })
      const region = await screen.findByRole('region', { name: 'Client setup' })
      return { section: within(region) }
    })()
    expect(await section.findByText(/private certificate authority/)).toBeInTheDocument()
    expect(section.getByText(/base URL is not configured/)).toBeInTheDocument()
    expect(section.getByRole('link', { name: 'Set the base URL' })).toHaveAttribute(
      'href',
      '/admin/settings',
    )
    expect(section.getByText('Mirror disabled by policy.')).toBeInTheDocument()
    // No template for the format: the generic URL is all there is.
    expect(section.getByText(/no client template yet/)).toBeInTheDocument()
  })

  it('shows the error with code and request id when the setup cannot be loaded', async () => {
    renderApp('/repositories/maven-releases', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/client-setup', () =>
          HttpResponse.json(
            { error: { code: 'internal_error', message: 'Something broke', request_id: 'req-cs' } },
            { status: 500, headers: { 'X-Request-Id': 'req-cs' } },
          ),
        ),
      ],
    })
    expect(await screen.findByText('The client setup could not be loaded')).toBeInTheDocument()
    expect(screen.getByText('internal_error')).toBeInTheDocument()
    expect(screen.getByText('req-cs')).toBeInTheDocument()
  })

  it('must_not_keep_the_new_token_in_the_page_text_or_in_browser_storage', async () => {
    const user = userEvent.setup()
    renderApp('/repositories/maven-releases', {
      user: 'admin',
      overrides: [
        http.post('/api/v1/me/tokens', () =>
          HttpResponse.json(
            {
              id: '0192f6c4-5a1e-7000-8000-00000000003d',
              user_id: '0192f6c4-5a1e-7000-8000-000000000001',
              name: 'client-setup-maven-releases',
              display_prefix: 'ryx_TEST',
              scopes: ['repo:maven-releases:browse'],
              expires_at: '2027-01-22T09:15:30Z',
              revoked_at: null,
              last_used_at: null,
              created_at: '2026-10-24T09:15:30Z',
              kind: 'personal',
              state: 'active',
              owner: null,
              never_used: true,
              project_id: null,
              secret: FAKE_TOKEN,
            },
            { status: 201 },
          ),
        ),
      ],
    })
    const region = await screen.findByRole('region', { name: 'Client setup' })
    await user.click(await within(region).findByRole('button', { name: 'Create a token for me' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create a token for these snippets' })
    await user.click(within(dialog).getByRole('button', { name: 'Create token' }))

    // The token is shown once, in the show-once dialog, which cannot be closed before it is acknowledged.
    const once = await screen.findByRole('dialog', { name: /Copy your API token now/ })
    expect(within(once).getByText(FAKE_TOKEN)).toBeInTheDocument()
    await user.click(within(once).getByRole('checkbox'))
    await user.click(within(once).getByRole('button', { name: 'Done' }))
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())

    // The snippets use it for this view, but its text is not in the DOM until it is revealed.
    expect(
      await screen.findByText(/use your new token for this page view only/),
    ).toBeInTheDocument()
    expect(document.body.textContent).not.toContain(FAKE_TOKEN)
    expect(window.localStorage.length).toBe(0)
    for (let i = 0; i < window.sessionStorage.length; i += 1) {
      const key = window.sessionStorage.key(i) ?? ''
      expect(window.sessionStorage.getItem(key)).not.toContain(FAKE_TOKEN)
    }
    await user.click(within(region).getAllByRole('button', { name: 'Show value' })[0]!)
    expect(document.body.textContent).toContain(FAKE_TOKEN)

    // Removing it puts the placeholders back and takes the token out of the page.
    await user.click(
      within(region).getByRole('button', { name: 'Remove the token from the snippets' }),
    )
    expect(document.body.textContent).not.toContain(FAKE_TOKEN)
  })

  it('shows an error inside the token dialog and keeps the dialog open when creation fails', async () => {
    const user = userEvent.setup()
    renderApp('/repositories/maven-releases', {
      user: 'admin',
      overrides: [
        http.post('/api/v1/me/tokens', () =>
          HttpResponse.json(
            {
              error: { code: 'rate_limited', message: 'Too many requests', request_id: 'req-tok' },
            },
            { status: 429, headers: { 'X-Request-Id': 'req-tok', 'Retry-After': '30' } },
          ),
        ),
      ],
    })
    const region = await screen.findByRole('region', { name: 'Client setup' })
    await user.click(await within(region).findByRole('button', { name: 'Create a token for me' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create a token for these snippets' })
    await user.click(within(dialog).getByRole('button', { name: 'Create token' }))
    expect(await within(dialog).findByText('The token could not be created')).toBeInTheDocument()
    expect(within(dialog).getByText('rate_limited')).toBeInTheDocument()
  })

  it('validates the token form before it asks the server', async () => {
    const user = userEvent.setup()
    renderApp('/repositories/maven-releases', { user: 'admin' })
    const region = await screen.findByRole('region', { name: 'Client setup' })
    await user.click(await within(region).findByRole('button', { name: 'Create a token for me' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create a token for these snippets' })
    const days = within(dialog).getByLabelText(/Expires in/)
    await user.clear(days)
    await user.type(days, '9999')
    await user.click(within(dialog).getByRole('button', { name: 'Create token' }))
    expect(
      await within(dialog).findByText(/whole number of days between 1 and 365/),
    ).toBeInTheDocument()
    expect(within(dialog).getByRole('radio', { name: /Read and publish/ })).toBeInTheDocument()
  })
})
