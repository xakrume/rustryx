import { configure, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'
import { ownComponents, resetContent } from '@/features/components/mock-content'
import { resetRepositories } from '@/features/repositories/mock-state'
import { repositories } from '@/mocks/store'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import { checksumParam, hasCriteria, isChecksum } from './api'

// The first renders of a file are slow when many test files run at once.
configure({ asyncUtilTimeout: 10_000 })

beforeEach(() => {
  resetRepositories()
  resetContent()
})

const results = () => screen.findByRole('region', { name: 'Component results' })

describe('search', () => {
  it('starts with examples instead of an empty table and runs one on activation', async () => {
    const user = userEvent.setup()
    const { router, container } = renderApp('/search', { user: 'admin' })
    expect(await screen.findByText('Search your repositories')).toBeInTheDocument()
    await expectNoA11yViolations(container)
    await user.click(screen.getByRole('link', { name: 'name:api-core' }))
    await waitFor(() => expect(router.state.location.search).toMatchObject({ name: 'api-core' }))
    const table = within(await results())
    expect((await table.findAllByRole('link', { name: /api-core$/ })).length).toBeGreaterThan(0)
  })

  it('runs on Enter, keeps the criteria in the address bar and shows them as removable chips', async () => {
    const user = userEvent.setup()
    const { router } = renderApp('/search', { user: 'admin' })
    await user.type(await screen.findByLabelText('Keyword'), 'api-core{Enter}')
    await waitFor(() => expect(router.state.location.search).toMatchObject({ q: 'api-core' }))
    const table = within(await results())
    await table.findAllByRole('link', { name: /api-core$/ })
    await user.selectOptions(screen.getByLabelText('Format'), 'maven2')
    await user.click(screen.getByRole('button', { name: 'Search' }))
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({ q: 'api-core', format: 'maven2' }),
    )
    const chips = within(screen.getByRole('list', { name: 'Search criteria in use' }))
    expect(chips.getByText('maven2')).toBeInTheDocument()
    await user.click(chips.getByRole('button', { name: 'Remove criterion Format' }))
    await waitFor(() => expect(router.state.location.search.format).toBeUndefined())
    expect(screen.getByLabelText('Format')).toHaveValue('')
  })

  it('names the fields of the chosen format', async () => {
    const user = userEvent.setup()
    renderApp('/search', { user: 'admin' })
    await user.selectOptions(await screen.findByLabelText('Format'), 'maven2')
    expect(screen.getByLabelText('Group ID')).toBeInTheDocument()
    expect(screen.getByLabelText('Artifact ID')).toBeInTheDocument()
    expect(screen.getByText(/Maven coordinates/)).toBeInTheDocument()
    await user.selectOptions(screen.getByLabelText('Format'), 'docker')
    expect(screen.getByLabelText('Image name')).toBeInTheDocument()
    expect(screen.getByLabelText('Tag')).toBeInTheDocument()
    expect(screen.getByLabelText('Digest')).toBeInTheDocument()
    expect(screen.queryByLabelText('Group ID')).not.toBeInTheDocument()
  })

  it('restores a saved search from the address bar, including the short form of the repository', async () => {
    renderApp('/search?q=api*&repo=maven-releases&format=maven2', { user: 'admin' })
    expect(await screen.findByLabelText('Keyword')).toHaveValue('api*')
    expect(screen.getByLabelText('Repository')).toHaveValue('maven-releases')
    const table = within(await results())
    const links = await table.findAllByRole('link', { name: /^com\.example\./ })
    expect(links.length).toBeGreaterThan(0)
    const cells = table.getAllByRole('cell')
    expect(cells.some((c) => c.textContent === 'maven-releases')).toBe(true)
    expect(cells.some((c) => c.textContent === 'maven-snapshots')).toBe(false)
  })

  it('finds a file by its checksum in the file results', async () => {
    const user = userEvent.setup()
    const asset = ownComponents(repositories.find((r) => r.name === 'maven-releases')!)[0]!
      .assets[0]!
    renderApp('/search', { user: 'admin' })
    await user.click(await screen.findByRole('radio', { name: 'Files' }))
    await user.type(screen.getByLabelText('Checksum'), `${asset.sha256.toUpperCase()}{Enter}`)
    const table = within(await screen.findByRole('region', { name: 'File results' }))
    expect(await table.findByRole('link', { name: asset.path })).toBeInTheDocument()
    expect(table.getAllByRole('row')).toHaveLength(2)
    expect(screen.getByLabelText('Path')).toBeInTheDocument()
  })

  it('must_not_ask_the_server_with_a_checksum_that_cannot_be_one', async () => {
    const user = userEvent.setup()
    let asked = 0
    renderApp('/search', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/search/components', () => {
          asked += 1
          return undefined
        }),
      ],
    })
    await user.type(await screen.findByLabelText('Checksum'), 'abc123{Enter}')
    expect(
      await screen.findByText('A checksum is 32, 40 or 64 hexadecimal characters.'),
    ).toBeInTheDocument()
    expect(asked).toBe(0)
  })

  it('explains a wildcard that is too short next to the keyword field, not as a failed list', async () => {
    const user = userEvent.setup()
    renderApp('/search', { user: 'admin' })
    await user.type(await screen.findByLabelText('Keyword'), 'ap*{Enter}')
    expect(await screen.findByText('The search was not accepted')).toBeInTheDocument()
    expect(screen.getByText(/needs at least 3 characters before the \*/)).toBeInTheDocument()
    expect(screen.getByText('Use at least 3 characters before the *.')).toBeInTheDocument()
    expect(screen.queryByRole('region', { name: 'Component results' })).not.toBeInTheDocument()
  })

  it('suggests what to try when nothing matches', async () => {
    const user = userEvent.setup()
    renderApp('/search', { user: 'admin' })
    await user.type(await screen.findByLabelText('Keyword'), 'no-such-thing-anywhere{Enter}')
    expect(await screen.findByText('No results')).toBeInTheDocument()
    expect(screen.getByText(/trailing \* for a prefix/)).toBeInTheDocument()
  })

  it('pages with load more', async () => {
    const user = userEvent.setup()
    renderApp('/search?q=api*', { user: 'admin' })
    await results()
    expect(await screen.findByText('50 rows')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Load more' }))
    expect(await screen.findByText(/^(100|\d{3}) rows/)).toBeInTheDocument()
  })

  it('says that only the first 1000 hits are reachable', async () => {
    renderApp('/search?q=api*', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/search/components', () =>
          HttpResponse.json({ items: [], next: null, truncated: true }),
        ),
      ],
    })
    expect(await screen.findByText(/Only the first 1000 hits/)).toBeInTheDocument()
  })

  it('shows a failed search with its code and request id', async () => {
    renderApp('/search?q=api', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/search/components', () =>
          HttpResponse.json(
            {
              error: {
                code: 'store_unavailable',
                message: 'The database is not reachable',
                request_id: 'req-s',
              },
            },
            { status: 503, headers: { 'X-Request-Id': 'req-s', 'Retry-After': '30' } },
          ),
        ),
      ],
    })
    expect(await screen.findByText('The list could not be loaded')).toBeInTheDocument()
    expect(screen.getByText('store_unavailable')).toBeInTheDocument()
    expect(screen.getByText('req-s')).toBeInTheDocument()
  })

  it('opens the detail drawer of a hit with the component and its client setup', async () => {
    const user = userEvent.setup()
    renderApp('/search?name=api-core&repository=maven-releases', { user: 'admin' })
    const table = within(await results())
    const button = (await table.findAllByRole('button', { name: /^Show details of / }))[0]!
    await user.click(button)
    const drawer = await screen.findByRole('dialog')
    expect(
      await within(drawer).findByRole('link', { name: 'Open the component page' }),
    ).toBeInTheDocument()
    expect(within(drawer).getByText('Client setup')).toBeInTheDocument()
    await user.keyboard('{Escape}')
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    expect(button).toHaveFocus()
  })

  it('opens the detail drawer of a file hit', async () => {
    const user = userEvent.setup()
    const asset = ownComponents(repositories.find((r) => r.name === 'maven-releases')!)[0]!
      .assets[0]!
    renderApp(`/search?kind=assets&path=${asset.path}`, { user: 'admin' })
    const table = within(await screen.findByRole('region', { name: 'File results' }))
    await user.click(await table.findByRole('button', { name: `Show details of ${asset.path}` }))
    const drawer = await screen.findByRole('dialog')
    expect(await within(drawer).findByText(asset.sha256)).toBeInTheDocument()
  })

  it('shows only what the caller may browse', async () => {
    renderApp('/search?q=api*&repository=maven-releases', {})
    // maven-releases is private: an anonymous visitor gets no hit, and no hint that it exists.
    expect(await screen.findByText('No results')).toBeInTheDocument()
  })

  it('resets to an empty search', async () => {
    const user = userEvent.setup()
    const { router } = renderApp('/search?q=api-core&sort=name', { user: 'admin' })
    await results()
    await user.click(screen.getByRole('button', { name: 'Reset' }))
    await waitFor(() => expect(router.state.location.search.q).toBeUndefined())
    expect(await screen.findByText('Search your repositories')).toBeInTheDocument()
  })
})

describe('search criteria', () => {
  it('needs at least one criterion, and the sort does not count', () => {
    expect(hasCriteria({ kind: 'components' })).toBe(false)
    expect(hasCriteria({ kind: 'components', sort: 'name' })).toBe(false)
    expect(hasCriteria({ kind: 'components', q: ' ' })).toBe(true)
    expect(hasCriteria({ kind: 'assets', path: 'a/*' })).toBe(true)
  })

  it('tells the digest by its length and refuses anything else', () => {
    const hex = (n: number) => 'a1'.repeat(n / 2)
    expect(checksumParam(hex(64))).toEqual({ sha256: hex(64) })
    expect(checksumParam(hex(40).toUpperCase())).toEqual({ sha1: hex(40) })
    expect(checksumParam(hex(32))).toEqual({ md5: hex(32) })
    expect(checksumParam(hex(30))).toEqual({})
    expect(checksumParam('zz'.repeat(32))).toEqual({})
    expect(checksumParam(undefined)).toEqual({})
    expect(isChecksum(hex(64))).toBe(true)
    expect(isChecksum('abc')).toBe(false)
  })
})
