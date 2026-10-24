import { configure, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'
import { resetContent } from '@/features/components/mock-content'
import { repositories } from '@/mocks/store'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import { bumpVersion, resetRepositories } from './mock-state'

// The first renders of a file are slow when many test files run at once.
configure({ asyncUtilTimeout: 10_000 })

beforeEach(() => {
  resetRepositories()
  resetContent()
})

const stored = (name: string) => repositories.find((r) => r.name === name)!

/** Spy on the PATCH body and let the mock answer: a handler that returns nothing falls through. */
function spy(bodies: unknown[]) {
  return http.patch('/api/v1/repositories/:name', async ({ request }) => {
    bodies.push(await request.clone().json())
  })
}

async function open(path: string, overrides: Parameters<typeof renderApp>[1] = { user: 'admin' }) {
  const result = renderApp(path, overrides)
  await screen.findByRole('button', { name: 'Save' })
  return result
}

describe('repository settings', () => {
  it('shows the locked identity and the editable settings of a hosted repository', async () => {
    const { container } = await open('/repositories/maven-releases/settings')
    expect(screen.getByRole('heading', { name: 'Fixed after creation' })).toBeInTheDocument()
    // The blob store is shown by name and locked.
    const store = screen.getByLabelText('Blob store')
    expect(store).toHaveValue('default')
    expect(store).toHaveAttribute('readonly')
    expect(screen.getByLabelText(/^Write policy/)).toHaveValue('allow_once')
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled()
    expect(screen.getByText('No changes to save.')).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('saves only what changed, with the version that was read, and shows the new state', async () => {
    const user = userEvent.setup()
    const bodies: unknown[] = []
    await open('/repositories/maven-releases/settings', { user: 'admin', overrides: [spy(bodies)] })
    await user.selectOptions(screen.getByLabelText(/^Write policy/), 'allow')
    await user.click(screen.getByRole('switch', { name: 'Allow anonymous read' }))
    expect(screen.getByText(/Everyone who can reach the server/)).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Save' }))
    expect(await screen.findByText('Settings of maven-releases saved.')).toBeInTheDocument()
    expect(bodies).toEqual([{ write_policy: 'allow', allow_anonymous: true }])
    expect(stored('maven-releases')).toMatchObject({ write_policy: 'allow', allow_anonymous: true })
    await waitFor(() => expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled())
  })

  it('discards edits on request', async () => {
    const user = userEvent.setup()
    await open('/repositories/maven-releases/settings')
    await user.selectOptions(screen.getByLabelText(/^Write policy/), 'deny')
    await user.click(screen.getByRole('button', { name: 'Discard changes' }))
    expect(screen.getByLabelText(/^Write policy/)).toHaveValue('allow_once')
  })

  it('must_show_the_conflict_dialog_when_the_version_is_stale', async () => {
    const user = userEvent.setup()
    await open('/repositories/maven-releases/settings')
    await user.selectOptions(screen.getByLabelText(/^Write policy/), 'allow')
    // Someone else saved meanwhile: the ETag this form holds is stale now.
    bumpVersion(stored('maven-releases'))
    await user.click(screen.getByRole('button', { name: 'Save' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(dialog).toHaveTextContent('Changed by someone else')
    expect(stored('maven-releases').write_policy).toBe('allow_once')
    // Overwrite repeats the edit against the version that is current now.
    await user.click(within(dialog).getByRole('button', { name: 'Overwrite with my changes' }))
    await waitFor(() => expect(stored('maven-releases').write_policy).toBe('allow'))
    expect(await screen.findByText('Settings of maven-releases saved.')).toBeInTheDocument()
  })

  it('reloads the latest version and drops the edit when asked to', async () => {
    const user = userEvent.setup()
    await open('/repositories/maven-releases/settings')
    await user.selectOptions(screen.getByLabelText(/^Write policy/), 'deny')
    bumpVersion(stored('maven-releases'))
    await user.click(screen.getByRole('button', { name: 'Save' }))
    const dialog = await screen.findByRole('alertdialog')
    await user.click(within(dialog).getByRole('button', { name: 'Reload the latest version' }))
    await waitFor(() => expect(screen.getByLabelText(/^Write policy/)).toHaveValue('allow_once'))
    expect(stored('maven-releases').write_policy).toBe('allow_once')
  })

  it('must_not_render_or_resend_the_stored_upstream_password', async () => {
    const user = userEvent.setup()
    const bodies: unknown[] = []
    await open('/repositories/npm-proxy/settings', { user: 'admin', overrides: [spy(bodies)] })
    // Give the proxy stored credentials, as a repository saved earlier would have.
    expect(screen.getByLabelText('Password')).toHaveValue('')
    expect(screen.getByLabelText('Password')).toHaveAttribute('type', 'password')
    expect(document.body.textContent).not.toMatch(/secret|an upstream password/)
    await user.clear(screen.getByLabelText('Content max age (seconds)'))
    await user.type(screen.getByLabelText('Content max age (seconds)'), '7200')
    expect(screen.getByText('That is 2 h.')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Save' }))
    await screen.findByText('Settings of npm-proxy saved.')
    // An edit that does not touch the credentials sends no `auth` at all, so the stored one is kept.
    expect(bodies).toEqual([{ proxy: { content_max_age_secs: 7200 } }])
  })

  it('sends a new password when one is typed and lets the credentials be removed', async () => {
    const user = userEvent.setup()
    const bodies: unknown[] = []
    const proxy = stored('npm-proxy')
    const index = repositories.indexOf(proxy)
    repositories.splice(index, 1, {
      ...proxy,
      proxy: { ...proxy.proxy!, auth: { username: 'reader', password_set: true } },
    })
    await open('/repositories/npm-proxy/settings', { user: 'admin', overrides: [spy(bodies)] })
    expect(screen.getByLabelText('Password')).toHaveAttribute('placeholder', 'A password is stored')
    expect(screen.getByText('Credentials set')).toBeInTheDocument()
    await user.type(screen.getByLabelText('Password'), 'rotated password')
    await user.click(screen.getByRole('button', { name: 'Save' }))
    await screen.findByText('Settings of npm-proxy saved.')
    expect(bodies[0]).toEqual({
      proxy: { auth: { username: 'reader', password: 'rotated password' } },
    })
    expect(JSON.stringify(stored('npm-proxy'))).not.toContain('rotated password')

    await waitFor(() => expect(screen.getByLabelText('Password')).toHaveValue(''))
    await user.click(screen.getByRole('checkbox', { name: 'Remove the stored credentials' }))
    await user.click(screen.getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(bodies).toHaveLength(2))
    expect(bodies[1]).toEqual({ proxy: { auth: null } })
    expect(stored('npm-proxy').proxy?.auth).toBeNull()
  })

  it('puts the server field errors on the fields', async () => {
    const user = userEvent.setup()
    await open('/repositories/npm-proxy/settings', {
      user: 'admin',
      overrides: [
        http.patch('/api/v1/repositories/:name', () =>
          HttpResponse.json(
            {
              error: {
                code: 'invalid_input',
                message: 'The request is not valid',
                request_id: 'req-fe',
                details: {
                  fields: [
                    {
                      field: '/proxy/metadata_max_age_secs',
                      code: 'too_big',
                      message: 'Too large for this server',
                    },
                  ],
                },
              },
            },
            { status: 400, headers: { 'X-Request-Id': 'req-fe' } },
          ),
        ),
      ],
    })
    await user.clear(screen.getByLabelText('Metadata max age (seconds)'))
    await user.type(screen.getByLabelText('Metadata max age (seconds)'), '60')
    await user.click(screen.getByRole('button', { name: 'Save' }))
    expect(await screen.findByText('Too large for this server')).toBeInTheDocument()
    expect(screen.getByLabelText('Metadata max age (seconds)')).toHaveFocus()
  })

  it('validates before it sends: a proxy needs a valid URL and sane numbers', async () => {
    const user = userEvent.setup()
    await open('/repositories/npm-proxy/settings')
    await user.clear(screen.getByLabelText(/^Remote URL/))
    await user.type(screen.getByLabelText(/^Remote URL/), 'ftp://nope')
    await user.clear(screen.getByLabelText('Concurrent upstream requests'))
    await user.type(screen.getByLabelText('Concurrent upstream requests'), '9000')
    await user.click(screen.getByRole('button', { name: 'Save' }))
    expect(await screen.findByText('Enter a full http or https address.')).toBeInTheDocument()
    expect(screen.getByText('Enter a number from 1 to 256.')).toBeInTheDocument()
  })

  it('adds a member to a group and keeps the order', async () => {
    const user = userEvent.setup()
    await open('/repositories/npm-group/settings')
    await screen.findByRole('option', { name: 'npm-snapshots' })
    await user.selectOptions(screen.getByLabelText('Add a member'), 'npm-snapshots')
    await user.click(screen.getByRole('button', { name: 'Add' }))
    await user.click(screen.getByRole('button', { name: 'Save' }))
    await screen.findByText('Settings of npm-group saved.')
    expect(stored('npm-group').group?.members).toEqual(['npm-releases', 'npm-snapshots'])
  })

  it('names a group cycle as a conflict and changes nothing', async () => {
    const user = userEvent.setup()
    const inner = stored('npm-group')
    // npm-outer already contains npm-group: adding npm-outer to npm-group would close a loop.
    repositories.push({
      ...inner,
      id: '0192f6c4-5a1e-7000-8000-0000000fffff',
      name: 'npm-outer',
      group: { members: ['npm-group'] },
    })
    await open('/repositories/npm-group/settings')
    await screen.findByRole('option', { name: 'npm-outer' })
    await user.selectOptions(screen.getByLabelText('Add a member'), 'npm-outer')
    await user.click(screen.getByRole('button', { name: 'Add' }))
    await user.click(screen.getByRole('button', { name: 'Save' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(dialog).toHaveTextContent('The change conflicts with the current state')
    expect(dialog).toHaveTextContent('npm-outer')
    expect(stored('npm-group').group?.members).toEqual(['npm-releases'])
  })

  it('lets an administrator invalidate the cache and block the upstream of a proxy', async () => {
    const user = userEvent.setup()
    await open('/repositories/npm-proxy/settings')
    await user.click(screen.getByRole('button', { name: 'Invalidate cache' }))
    expect(await screen.findByText('The cache of npm-proxy is stale now.')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Block upstream' }))
    expect(await screen.findByText('The upstream of npm-proxy is blocked.')).toBeInTheDocument()
    expect(stored('npm-proxy').upstream?.state).toBe('blocked')
    await user.click(await screen.findByRole('button', { name: 'Unblock upstream' }))
    expect(await screen.findByText('The upstream of npm-proxy is unblocked.')).toBeInTheDocument()
  })

  it('warns before leaving with unsaved edits', async () => {
    const user = userEvent.setup()
    await open('/repositories/maven-releases/settings')
    await user.selectOptions(screen.getByLabelText(/^Write policy/), 'deny')
    await user.click(screen.getByRole('link', { name: 'Overview' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(dialog).toHaveTextContent('Discard unsaved changes?')
    await user.click(within(dialog).getByRole('button', { name: 'Discard and leave' }))
    expect(await screen.findByRole('heading', { name: 'Summary' })).toBeInTheDocument()
  })

  it('deletes a repository from the danger zone after the typed confirmation', async () => {
    const user = userEvent.setup()
    await open('/repositories/team-005-raw/settings')
    await user.click(screen.getByRole('button', { name: 'Delete repository' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(dialog).toHaveTextContent('Delete repository team-005-raw?')
    await user.type(within(dialog).getByRole('textbox'), 'team-005-raw')
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }))
    expect(
      await screen.findByRole('heading', { level: 1, name: 'Repositories' }),
    ).toBeInTheDocument()
    expect(repositories.some((r) => r.name === 'team-005-raw')).toBe(false)
  })

  it('must_open_the_forbidden_page_for_someone_who_may_not_administer', async () => {
    renderApp('/repositories/maven-releases/settings', { user: 'alice' })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Save' })).not.toBeInTheDocument()
  })
})
