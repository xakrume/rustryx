import { configure, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'
import { resetContent } from '@/features/components/mock-content'
import { repositories } from '@/mocks/store'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import { MOCK_FORMATS } from './mock-formats'
import { resetRepositories } from './mock-state'

// The first renders of a file are slow when many test files run at once.
configure({ asyncUtilTimeout: 10_000 })

beforeEach(() => {
  resetRepositories()
  resetContent()
})

type User = ReturnType<typeof userEvent.setup>

async function step1(user: User, name: string, format: RegExp, type?: 'Proxy' | 'Group') {
  const field = await screen.findByLabelText(/^Name/)
  await user.type(field, name)
  await user.click(await screen.findByRole('radio', { name: format }))
  if (type) await user.click(screen.getByRole('radio', { name: new RegExp(`^${type}`) }))
}

// The navigation has headings of its own ("Storage"), so look inside the page.
const page = () => within(screen.getByRole('main'))

const next = (user: User) => user.click(screen.getByRole('button', { name: /^Next/ }))

describe('create repository wizard', () => {
  it('creates a hosted repository through the three steps and opens it', async () => {
    const user = userEvent.setup()
    const { container } = renderApp('/repositories/new', { user: 'admin' })
    await step1(user, 'my-new-repo', /Maven 2/)
    expect(await screen.findByText('This name is available.')).toBeInTheDocument()
    await expectNoA11yViolations(container)

    await next(user)
    expect(
      await within(await screen.findByRole('main')).findByRole('heading', { name: 'Storage' }),
    ).toBeInTheDocument()
    // Maven's own options come from the schema the server sent, with its defaults.
    expect(screen.getByLabelText('Version policy')).toHaveValue('release')
    expect(screen.getByLabelText(/^Write policy/)).toHaveValue('allow_once')
    await user.selectOptions(screen.getByLabelText('Layout policy'), 'permissive')
    await next(user)

    expect(await screen.findByRole('heading', { name: 'Review' })).toBeInTheDocument()
    expect(screen.getByText('my-new-repo')).toBeInTheDocument()
    expect(screen.getByText('Permissive')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Create repository' }))

    expect(
      await screen.findByRole('heading', { level: 1, name: 'my-new-repo' }),
    ).toBeInTheDocument()
    const created = repositories.find((r) => r.name === 'my-new-repo')
    expect(created).toMatchObject({
      format: 'maven2',
      repo_type: 'hosted',
      write_policy: 'allow_once',
    })
    expect(created?.format_config).toMatchObject({ layout_policy: 'permissive' })
  })

  it('refuses a name that is taken, live, before the server is asked to create it', async () => {
    const user = userEvent.setup()
    renderApp('/repositories/new', { user: 'admin' })
    await step1(user, 'maven-releases', /Maven 2/)
    expect(
      await screen.findByText('A repository with this name already exists.'),
    ).toBeInTheDocument()
    await next(user)
    expect(page().queryByRole('heading', { name: 'Storage' })).not.toBeInTheDocument()
    expect(screen.getByLabelText(/^Name/)).toHaveFocus()
  })

  it('checks each step before it lets you move on', async () => {
    const user = userEvent.setup()
    renderApp('/repositories/new', { user: 'admin' })
    await screen.findByLabelText(/^Name/)
    await next(user)
    expect(await screen.findByText('Enter a name for the repository.')).toBeInTheDocument()
    expect(screen.getByText('Choose a format.')).toBeInTheDocument()
    await user.type(screen.getByLabelText(/^Name/), 'bad name')
    await user.click(screen.getByRole('radio', { name: /npm/ }))
    await next(user)
    expect(await screen.findByText(/Use letters, digits, dots/)).toBeInTheDocument()
  })

  it('must_not_create_the_repository_when_enter_is_pressed_before_the_last_step', async () => {
    const user = userEvent.setup()
    renderApp('/repositories/new', { user: 'admin' })
    await step1(user, 'enter-test', /npm/)
    await user.type(screen.getByLabelText(/^Name/), '{Enter}')
    expect(
      await within(await screen.findByRole('main')).findByRole('heading', { name: 'Storage' }),
    ).toBeInTheDocument()
    expect(repositories.some((r) => r.name === 'enter-test')).toBe(false)
  })

  it('disables a type the format does not support and says why', async () => {
    const user = userEvent.setup()
    renderApp('/repositories/new', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/formats', () =>
          HttpResponse.json({
            items: MOCK_FORMATS.map((f) =>
              f.format === 'raw'
                ? { ...f, supports: { hosted: true, proxy: true, group: false } }
                : f,
            ),
            next: null,
          }),
        ),
      ],
    })
    await user.click(await screen.findByRole('radio', { name: /^Raw/ }))
    const group = screen.getByRole('radio', { name: /^Group/ })
    expect(group).toBeDisabled()
    expect(screen.getByText('Not available for Raw.')).toBeInTheDocument()
    expect(screen.getByRole('radio', { name: /^Hosted/ })).toBeEnabled()
  })

  it('creates a proxy repository and sends the credentials with a write only password', async () => {
    const user = userEvent.setup()
    renderApp('/repositories/new', { user: 'admin' })
    await step1(user, 'npm-mirror', /npm/, 'Proxy')
    await next(user)
    await user.type(await screen.findByLabelText(/^Remote URL/), 'https://registry.example.org')
    await user.type(screen.getByLabelText('User name'), 'mirror-reader')
    const password = screen.getByLabelText('Password')
    expect(password).toHaveAttribute('type', 'password')
    await user.type(password, 'an upstream password')
    await next(user)
    // The review says a password is set and never shows it.
    expect(await screen.findByText(/mirror-reader/)).toBeInTheDocument()
    expect(screen.getByText(/password set/)).toBeInTheDocument()
    expect(document.body.textContent).not.toContain('an upstream password')
    await user.click(screen.getByRole('button', { name: 'Create repository' }))
    await screen.findByRole('heading', { level: 1, name: 'npm-mirror' })
    const created = repositories.find((r) => r.name === 'npm-mirror')
    expect(created?.proxy?.auth).toEqual({ username: 'mirror-reader', password_set: true })
    expect(JSON.stringify(created)).not.toContain('an upstream password')
  })

  it('returns to the settings step with the SSRF rule when the server refuses the remote URL', async () => {
    const user = userEvent.setup()
    renderApp('/repositories/new', { user: 'admin' })
    await step1(user, 'bad-upstream', /npm/, 'Proxy')
    await next(user)
    await user.type(await screen.findByLabelText(/^Remote URL/), 'http://localhost:9000')
    await next(user)
    await user.click(await screen.findByRole('button', { name: 'Create repository' }))
    expect(
      await screen.findByText(/The server refused this address \(rule: private_address\)/),
    ).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Upstream' })).toBeInTheDocument()
    expect(screen.getByLabelText(/^Remote URL/)).toHaveFocus()
    expect(repositories.some((r) => r.name === 'bad-upstream')).toBe(false)
  })

  it('warns, but does not block, when the connection test fails', async () => {
    const user = userEvent.setup()
    renderApp('/repositories/new', { user: 'admin' })
    await step1(user, 'probe-test', /npm/, 'Proxy')
    await next(user)
    await user.type(
      await screen.findByLabelText(/^Remote URL/),
      'https://registry.down.example.org',
    )
    await user.click(screen.getByRole('button', { name: 'Test connection' }))
    expect(await screen.findByText('The upstream did not answer as expected')).toBeInTheDocument()
    expect(screen.getByText('upstream_timeout')).toBeInTheDocument()
    await next(user)
    expect(await screen.findByRole('heading', { name: 'Review' })).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Back' }))
    await user.clear(screen.getByLabelText(/^Remote URL/))
    await user.type(screen.getByLabelText(/^Remote URL/), 'https://registry.example.org')
    await user.click(screen.getByRole('button', { name: 'Test connection' }))
    expect(await screen.findByText(/answered 200 in 143 ms/)).toBeInTheDocument()
  })

  it('creates a group with its members in the order chosen', async () => {
    const user = userEvent.setup()
    renderApp('/repositories/new', { user: 'admin' })
    await step1(user, 'maven-all', /Maven 2/, 'Group')
    await next(user)
    expect(await screen.findByRole('heading', { name: 'Members' })).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Next' }))
    expect(await screen.findByText('A group needs at least one member.')).toBeInTheDocument()
    await user.selectOptions(screen.getByLabelText('Add a member'), 'maven-proxy')
    await user.click(screen.getByRole('button', { name: 'Add' }))
    await user.selectOptions(screen.getByLabelText('Add a member'), 'maven-releases')
    await user.click(screen.getByRole('button', { name: 'Add' }))
    await user.click(screen.getByRole('button', { name: 'Move maven-releases up' }))
    await next(user)
    await user.click(await screen.findByRole('button', { name: 'Create repository' }))
    await screen.findByRole('heading', { level: 1, name: 'maven-all' })
    expect(repositories.find((r) => r.name === 'maven-all')?.group?.members).toEqual([
      'maven-releases',
      'maven-proxy',
    ])
  })

  it('asks before leaving the form with unsaved input', async () => {
    const user = userEvent.setup()
    renderApp('/repositories/new', { user: 'admin' })
    await user.type(await screen.findByLabelText(/^Name/), 'half-done')
    await user.click(screen.getByRole('link', { name: 'Home' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(dialog).toHaveTextContent('Discard unsaved changes?')
    await user.click(within(dialog).getByRole('button', { name: 'Keep editing' }))
    expect(screen.getByLabelText(/^Name/)).toHaveValue('half-done')
  })

  it('must_not_open_for_someone_who_may_not_create_repositories', async () => {
    renderApp('/repositories/new', { user: 'alice' })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
    await waitFor(() => expect(screen.queryByLabelText(/^Name/)).not.toBeInTheDocument())
  })
})
