import { configure, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import { resetBlobStoreMock } from './mocks'

// The whole suite runs in parallel workers: the first render of a page loads a route chunk and
// can take longer than the default one second on a busy machine.
configure({ asyncUtilTimeout: 5000 })

const envelope = (code: string, message: string, status: number, details?: unknown) =>
  HttpResponse.json(
    { error: { code, message, request_id: `req-${code}`, ...(details ? { details } : {}) } },
    { status, headers: { 'X-Request-Id': `req-${code}` } },
  )

describe('blob store list', () => {
  beforeEach(() => resetBlobStoreMock())

  it('lists the stores with usage meters, quota status and health in words', async () => {
    const { container } = renderApp('/admin/blob-stores', { user: 'admin' })
    const table = await screen.findByRole('region', { name: 'Blob stores' })
    expect(await within(table).findByRole('link', { name: 'default' })).toHaveAttribute(
      'href',
      '/admin/blob-stores/default',
    )
    const meters = within(table).getAllByRole('meter')
    expect(meters.length).toBeGreaterThanOrEqual(4)
    expect(within(table).getByRole('meter', { name: 'Used space of fast-ssd' })).toHaveAttribute(
      'aria-valuenow',
      '91',
    )
    // Status is text, not only a color: near quota, over the soft quota, degraded, unreachable.
    expect(within(table).getByText('Near quota')).toBeInTheDocument()
    expect(within(table).getByText('Over soft quota')).toBeInTheDocument()
    expect(within(table).getByText('Degraded')).toBeInTheDocument()
    expect(within(table).getByText('Unreachable')).toBeInTheDocument()
    expect(within(table).getByText('blob_store_unavailable')).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('draws the usage trend under the list when the server reports history', async () => {
    renderApp('/admin/blob-stores', { user: 'admin' })
    const figure = await screen.findByRole('figure')
    expect(within(figure).getByText('Storage used by blob store')).toBeInTheDocument()
    expect(within(figure).getByRole('button', { name: 'Show data table' })).toBeInTheDocument()
  })

  it('must_not_block_with_text_when_a_store_is_over_its_soft_quota', async () => {
    renderApp('/admin/blob-stores', { user: 'admin' })
    const table = await screen.findByRole('region', { name: 'Blob stores' })
    await within(table).findByText('Over soft quota')
    // The quota is soft: the page never says writes are refused.
    expect(screen.queryByText(/writes are (refused|blocked)/i)).not.toBeInTheDocument()
    expect(screen.queryByText(/blocked/i)).not.toBeInTheDocument()
  })

  it('filters by kind from the address bar', async () => {
    renderApp('/admin/blob-stores?kind=s3', { user: 'admin' })
    const table = await screen.findByRole('region', { name: 'Blob stores' })
    expect(await within(table).findByRole('link', { name: 'archive-s3' })).toBeInTheDocument()
    expect(within(table).queryByRole('link', { name: 'default' })).not.toBeInTheDocument()
    expect(screen.getByRole('list', { name: 'Active filters' })).toHaveTextContent('S3 compatible')
  })

  it('shows the first use state with a create action', async () => {
    renderApp('/admin/blob-stores', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/blob-stores', () => HttpResponse.json({ items: [], next: null })),
      ],
    })
    expect(await screen.findByText('No blob stores yet')).toBeInTheDocument()
    expect(screen.getAllByRole('button', { name: 'Create blob store' }).length).toBeGreaterThan(0)
  })

  it('shows the error with code and request id, and retries', async () => {
    let calls = 0
    renderApp('/admin/blob-stores', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/blob-stores', () => {
          calls += 1
          return envelope('store_unavailable', 'The database is not reachable', 503)
        }),
      ],
    })
    expect(await screen.findByText('store_unavailable')).toBeInTheDocument()
    expect(screen.getByText('req-store_unavailable')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: 'Retry' }))
    await waitFor(() => expect(calls).toBeGreaterThan(1))
  })

  it('must_show_the_forbidden_panel_to_a_non_administrator', async () => {
    renderApp('/admin/blob-stores', { user: 'alice' })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
    expect(screen.queryByRole('region', { name: 'Blob stores' })).not.toBeInTheDocument()
  })

  it('shows the forbidden panel when the server refuses the list with 403', async () => {
    renderApp('/admin/blob-stores', {
      user: 'admin',
      overrides: [http.get('/api/v1/blob-stores', () => envelope('forbidden', 'Not allowed', 403))],
    })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
  })
})

describe('blob store create', () => {
  beforeEach(() => resetBlobStoreMock())

  it('creates a local store and lands on its page', async () => {
    const user = userEvent.setup()
    renderApp('/admin/blob-stores', { user: 'admin' })
    await user.click((await screen.findAllByRole('button', { name: 'Create blob store' }))[0]!)
    expect(await screen.findByRole('heading', { name: 'Create a blob store' })).toBeInTheDocument()
    await user.type(screen.getByLabelText(/^Name/), 'warm-ssd')
    await user.type(screen.getByLabelText(/^Directory/), '/mnt/warm/blobs')
    await user.type(screen.getByLabelText(/^Soft quota/), '250')
    await user.click(screen.getByRole('button', { name: 'Create blob store' }))
    expect(await screen.findByRole('heading', { level: 1, name: 'warm-ssd' })).toBeInTheDocument()
    expect(await screen.findByText('Blob store warm-ssd created')).toBeInTheDocument()
  })

  it('validates required fields next to the fields and focuses the first one', async () => {
    const user = userEvent.setup()
    renderApp('/admin/blob-stores?view=create', { user: 'admin' })
    await user.click(await screen.findByRole('button', { name: 'Create blob store' }))
    const name = screen.getByLabelText(/^Name/)
    expect(await screen.findByText('Enter a name.')).toBeInTheDocument()
    expect(screen.getByText('Enter the directory.')).toBeInTheDocument()
    expect(name).toHaveAttribute('aria-invalid', 'true')
    expect(name).toHaveFocus()
  })

  it('shows the S3 fields and the credential choices, and validates references', async () => {
    const user = userEvent.setup()
    renderApp('/admin/blob-stores?view=create', { user: 'admin' })
    await user.selectOptions(await screen.findByLabelText(/^Kind/), 's3')
    expect(screen.getByLabelText(/^Endpoint URL/)).toBeInTheDocument()
    await user.selectOptions(screen.getByLabelText(/^Credentials/), 'reference')
    await user.type(screen.getByLabelText(/^Access key reference/), 'ACCESS')
    await user.click(screen.getByRole('button', { name: 'Create blob store' }))
    expect(await screen.findAllByText('Use env:NAME or file:/absolute/path.')).not.toHaveLength(0)
    expect(screen.getByText('Enter the endpoint URL.')).toBeInTheDocument()
  })

  it('maps a taken name and a refused endpoint onto their fields', async () => {
    const user = userEvent.setup()
    renderApp('/admin/blob-stores?view=create', { user: 'admin' })
    await user.type(await screen.findByLabelText(/^Name/), 'default')
    await user.type(screen.getByLabelText(/^Directory/), '/mnt/x')
    await user.click(screen.getByRole('button', { name: 'Create blob store' }))
    expect(
      await screen.findByText('A blob store with this name already exists'),
    ).toBeInTheDocument()

    await user.clear(screen.getByLabelText(/^Name/))
    await user.type(screen.getByLabelText(/^Name/), 'cloud')
    await user.selectOptions(screen.getByLabelText(/^Kind/), 's3')
    await user.type(screen.getByLabelText(/^Endpoint URL/), 'http://169.254.169.254')
    await user.type(screen.getByLabelText(/^Bucket/), 'b')
    await user.type(screen.getByLabelText(/^Region/), 'us-east-1')
    await user.click(screen.getByRole('button', { name: 'Create blob store' }))
    expect(
      await screen.findByText('The endpoint address is not allowed (private_address)'),
    ).toBeInTheDocument()
  })

  it('must_not_render_the_s3_secret_key_after_save', async () => {
    const user = userEvent.setup()
    const secret = 'wJalrXUtnFEMI-secret-9f3a'
    const access = 'AKIAEXAMPLE0001'
    renderApp('/admin/blob-stores?view=create', { user: 'admin' })
    await user.type(await screen.findByLabelText(/^Name/), 'secure-s3')
    await user.selectOptions(screen.getByLabelText(/^Kind/), 's3')
    await user.type(screen.getByLabelText(/^Endpoint URL/), 'https://seaweedfs.example.org')
    await user.type(screen.getByLabelText(/^Bucket/), 'secure')
    await user.type(screen.getByLabelText(/^Region/), 'us-east-1')
    await user.selectOptions(screen.getByLabelText(/^Credentials/), 'inline')
    await user.type(screen.getByLabelText(/^Access key(?! reference)/), access)
    await user.type(screen.getByLabelText(/^Secret key(?! reference)/), secret)
    // While typing, the secret field is a password field, never plain text.
    expect(screen.getByLabelText(/^Secret key(?! reference)/)).toHaveAttribute('type', 'password')
    await user.click(screen.getByRole('button', { name: 'Create blob store' }))

    expect(await screen.findByRole('heading', { level: 1, name: 'secure-s3' })).toBeInTheDocument()
    await screen.findByText(/Credentials: configured/)
    const everything = document.body.innerHTML + document.body.textContent
    expect(everything).not.toContain(secret)
    expect(everything).not.toContain(access)
    for (const input of document.querySelectorAll('input')) {
      expect(input.value).not.toContain(secret)
      expect(input.value).not.toContain(access)
    }
  })
})

describe('blob store detail', () => {
  beforeEach(() => resetBlobStoreMock())

  it('shows usage, configuration, connection test and repositories', async () => {
    const { container } = renderApp('/admin/blob-stores/fast-ssd', { user: 'admin' })
    expect(await screen.findByRole('heading', { level: 1, name: 'fast-ssd' })).toBeInTheDocument()
    expect(await screen.findByRole('link', { name: 'maven-releases' })).toBeInTheDocument()
    expect(screen.getByLabelText(/^Directory/)).toHaveValue('/mnt/ssd/blobs')
    // Used by repositories: the directory cannot change and the page says why.
    expect(screen.getByLabelText(/^Directory/)).toHaveAttribute('readonly')
    expect(screen.getByText(/2 repositories use this store/)).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('tests the connection and says what happened, for a healthy and an unreachable store', async () => {
    const user = userEvent.setup()
    renderApp('/admin/blob-stores/fast-ssd', { user: 'admin' })
    await user.click(await screen.findByRole('button', { name: 'Test connection' }))
    expect(await screen.findByText('Connection works')).toBeInTheDocument()
    expect(screen.getByText(/succeeded in 12 ms/)).toBeInTheDocument()
  })

  it('reports a failed connection test with the stable code', async () => {
    const user = userEvent.setup()
    renderApp('/admin/blob-stores/cold-s3', { user: 'admin' })
    await user.click(await screen.findByRole('button', { name: 'Test connection' }))
    expect(await screen.findByText('Connection failed')).toBeInTheDocument()
    expect(screen.getByText(/Error code: blob_store_unavailable/)).toBeInTheDocument()
  })

  it('saves a changed quota with the ETag and shows the saved state', async () => {
    const user = userEvent.setup()
    let sentIfMatch: string | null = null
    renderApp('/admin/blob-stores/fast-ssd', {
      user: 'admin',
      overrides: [
        http.patch('/api/v1/blob-stores/fast-ssd', ({ request }) => {
          sentIfMatch = request.headers.get('if-match')
          return undefined
        }),
      ],
    })
    const quota = await screen.findByLabelText(/^Soft quota/)
    await user.clear(quota)
    await user.type(quota, '200')
    await user.click(screen.getByRole('button', { name: 'Save changes' }))
    expect(await screen.findByText('Blob store fast-ssd saved')).toBeInTheDocument()
    expect(sentIfMatch).toMatch(/^"blob-store-fast-ssd-v1"$/)
    await waitFor(() => expect(screen.getByLabelText(/^Soft quota/)).toHaveValue('200'))
  })

  it('must_show_the_conflict_dialog_when_the_etag_is_stale', async () => {
    const user = userEvent.setup()
    renderApp('/admin/blob-stores/fast-ssd', {
      user: 'admin',
      overrides: [
        http.patch('/api/v1/blob-stores/fast-ssd', () =>
          envelope('precondition_failed', 'The object was changed by someone else', 412),
        ),
      ],
    })
    const quota = await screen.findByLabelText(/^Soft quota/)
    await user.clear(quota)
    await user.type(quota, '300')
    await user.click(screen.getByRole('button', { name: 'Save changes' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText('Changed by someone else')).toBeInTheDocument()
    expect(
      within(dialog).getByRole('button', { name: 'Reload the latest version' }),
    ).toBeInTheDocument()
    expect(
      within(dialog).getByRole('button', { name: 'Overwrite with my changes' }),
    ).toBeInTheDocument()
  })

  it('must_not_put_credential_values_on_the_clipboard_with_copy_my_changes', async () => {
    const user = userEvent.setup()
    const written: string[] = []
    Object.defineProperty(navigator, 'clipboard', {
      value: { writeText: (text: string) => Promise.resolve(void written.push(text)) },
      configurable: true,
    })
    renderApp('/admin/blob-stores/archive-s3', {
      user: 'admin',
      overrides: [
        http.patch('/api/v1/blob-stores/archive-s3', () =>
          envelope('precondition_failed', 'Changed', 412),
        ),
      ],
    })
    await user.selectOptions(await screen.findByLabelText(/^Credentials/), 'inline')
    await user.type(screen.getByLabelText(/^Access key(?! reference)/), 'AKIANEW')
    await user.type(screen.getByLabelText(/^Secret key(?! reference)/), 'top-secret-value')
    await user.click(screen.getByRole('button', { name: 'Save changes' }))
    const dialog = await screen.findByRole('alertdialog')
    await user.click(within(dialog).getByRole('button', { name: 'Copy my changes' }))
    await waitFor(() => expect(written.length).toBe(1))
    expect(written[0]).not.toContain('top-secret-value')
    expect(written[0]).not.toContain('AKIANEW')
    expect(written[0]).toContain('[not copied]')
  })

  it('must_not_render_the_secret_key_after_replacing_credentials', async () => {
    const user = userEvent.setup()
    renderApp('/admin/blob-stores/archive-s3', { user: 'admin' })
    await user.selectOptions(await screen.findByLabelText(/^Credentials/), 'inline')
    await user.type(screen.getByLabelText(/^Access key(?! reference)/), 'AKIAREPLACED')
    await user.type(screen.getByLabelText(/^Secret key(?! reference)/), 'replaced-secret-7781')
    await user.click(screen.getByRole('button', { name: 'Save changes' }))
    expect(await screen.findByText('Blob store archive-s3 saved')).toBeInTheDocument()
    await waitFor(() =>
      expect(screen.queryByLabelText(/^Secret key(?! reference)/)).not.toBeInTheDocument(),
    )
    expect(document.body.innerHTML).not.toContain('replaced-secret-7781')
    expect(document.body.innerHTML).not.toContain('AKIAREPLACED')
  })

  it('must_block_deleting_a_blob_store_in_use_and_give_the_reason', async () => {
    renderApp('/admin/blob-stores/fast-ssd', { user: 'admin' })
    const button = await screen.findByRole('button', { name: 'Delete blob store' })
    expect(button).toBeDisabled()
    expect(screen.getByText('This blob store is in use')).toBeInTheDocument()
    expect(screen.getByText(/2 repositories store their content here/)).toBeInTheDocument()
    expect(button).toHaveAccessibleDescription(/maven-releases, docker-releases/)
  })

  it('deletes an unused store after the typed confirmation and returns to the list', async () => {
    const user = userEvent.setup()
    renderApp('/admin/blob-stores/scratch', { user: 'admin' })
    await user.click(await screen.findByRole('button', { name: 'Delete blob store' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(within(dialog).getByText(/Delete blob store/)).toHaveTextContent('scratch')
    const confirm = within(dialog).getByRole('button', { name: 'Delete' })
    expect(confirm).toBeDisabled()
    await user.type(within(dialog).getByLabelText('Confirmation text'), 'scratch')
    await user.click(confirm)
    expect(await screen.findByText('Blob store scratch deleted')).toBeInTheDocument()
    const table = await screen.findByRole('region', { name: 'Blob stores' })
    await waitFor(() =>
      expect(within(table).queryByRole('link', { name: 'scratch' })).not.toBeInTheDocument(),
    )
  })

  it('lists the dependents in the dialog when the server still answers 409', async () => {
    const user = userEvent.setup()
    renderApp('/admin/blob-stores/scratch', {
      user: 'admin',
      overrides: [
        http.delete('/api/v1/blob-stores/scratch', () =>
          envelope('resource_in_use', 'Repositories still use this blob store', 409, {
            dependents: [{ kind: 'repository', name: 'late-arrival' }],
          }),
        ),
      ],
    })
    await user.click(await screen.findByRole('button', { name: 'Delete blob store' }))
    const dialog = await screen.findByRole('alertdialog')
    await user.type(within(dialog).getByLabelText('Confirmation text'), 'scratch')
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }))
    expect(await within(dialog).findByText('late-arrival')).toBeInTheDocument()
  })

  it('shows not found for an unknown store', async () => {
    renderApp('/admin/blob-stores/nope', { user: 'admin' })
    expect(await screen.findByText('Not found or hidden from you')).toBeInTheDocument()
    expect(screen.getByText('blob_store_not_found')).toBeInTheDocument()
  })
})
