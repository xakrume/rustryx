import { configure, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'
import { ownComponents, resetContent } from '@/features/components/mock-content'
import { resetRepositories } from '@/features/repositories/mock-state'
import { repositories } from '@/mocks/store'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import { cleanPath, nodesOf, revealIdsFor } from './tree-nodes'

// The first renders of a file are slow when many test files run at once.
configure({ asyncUtilTimeout: 10_000 })

beforeEach(() => {
  resetRepositories()
  resetContent()
})

const component = () => ownComponents(repositories.find((r) => r.name === 'maven-releases')!)[0]!
const tree = () => screen.findByRole('tree', { name: 'Content of maven-releases' })

describe('browse tree', () => {
  it('opens folders lazily with the keyboard and shows what a scan needs on each row', async () => {
    const user = userEvent.setup()
    renderApp('/repositories/maven-releases/browse', { user: 'admin' })
    const root = await screen.findByRole('treeitem', { name: /maven-releases/ })
    // The root is open already; its first folder is "com" with its entry count.
    const com = await screen.findByRole('treeitem', { name: /com/ })
    expect(com).toHaveAttribute('aria-expanded', 'false')
    com.focus()
    await user.keyboard('{ArrowRight}')
    expect(await screen.findByRole('treeitem', { name: /example/ })).toBeInTheDocument()
    await user.keyboard('{ArrowLeft}')
    expect(com).toHaveAttribute('aria-expanded', 'false')
    expect(root).toHaveAttribute('aria-expanded', 'true')
  })

  it('selects a file, puts its path in the address bar and shows its details with copy buttons', async () => {
    const user = userEvent.setup()
    const asset = component().assets[0]!
    const { router, container } = renderApp(
      `/repositories/maven-releases/browse?path=/${asset.path}`,
      {
        user: 'admin',
      },
    )
    const row = await screen.findByRole('treeitem', {
      name: new RegExp(asset.path.split('/').pop()!),
    })
    expect(row).toHaveAttribute('aria-selected', 'true')
    const pane = within(await screen.findByRole('region', { name: 'Details of the selection' }))
    expect(await pane.findByText(asset.path)).toBeInTheDocument()
    expect(pane.getByRole('button', { name: 'Copy SHA-256 checksum' })).toBeInTheDocument()
    expect(pane.getByRole('link', { name: 'Open file page' })).toHaveAttribute(
      'href',
      `/assets/${asset.id}`,
    )
    // The component of the file is summarised with a link.
    expect(await pane.findByRole('heading', { name: 'Component' })).toBeInTheDocument()
    await expectNoA11yViolations(container)

    // Choosing another row in the tree changes the address bar and the pane.
    const other = component().assets[1]!
    await user.click(
      await screen.findByRole('treeitem', { name: new RegExp(other.path.split('/').pop()!) }),
    )
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({ path: `/${other.path}` }),
    )
    expect(await pane.findByText(other.path)).toBeInTheDocument()
  })

  it('shows the content path and opens a parent folder from it', async () => {
    const user = userEvent.setup()
    const asset = component().assets[0]!
    const { router } = renderApp(`/repositories/maven-releases/browse?path=/${asset.path}`, {
      user: 'admin',
    })
    const segments = asset.path.split('/')
    const nav = within(await screen.findByRole('navigation', { name: 'Content path' }))
    expect(nav.getByText(segments.at(-1)!)).toHaveAttribute('aria-current', 'page')
    const parent = segments.at(-2)!
    const button = nav.getAllByRole('button').find((el) => el.textContent === parent)
    expect(button).toBeDefined()
    await user.click(button!)
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({
        path: `/${segments.slice(0, -1).join('/')}/`,
      }),
    )
  })

  it('opens the folders on the way to a selection that came from the address bar', async () => {
    const asset = component().assets[0]!
    renderApp(`/repositories/maven-releases/browse?path=/${asset.path}`, { user: 'admin' })
    const segments = asset.path.split('/')
    for (const folder of segments.slice(0, -1)) {
      // Each folder opens once its parent's children are there, so wait for it level by level.
      const row = await screen.findByRole('treeitem', {
        name: new RegExp(`^${folder.replace('.', '\\.')}`),
      })
      await waitFor(() => expect(row).toHaveAttribute('aria-expanded', 'true'))
    }
  })

  it('shows a folder as a folder and says what to do next', async () => {
    renderApp('/repositories/maven-releases/browse?path=/com/example/', { user: 'admin' })
    const pane = within(await screen.findByRole('region', { name: 'Details of the selection' }))
    expect(await pane.findByText('/com/example/', { selector: 'dd' })).toBeInTheDocument()
    expect(pane.getByText(/Open a file in the tree/)).toBeInTheDocument()
    expect(pane.getByRole('button', { name: 'Copy folder path' })).toBeInTheDocument()
  })

  it('starts with nothing selected', async () => {
    renderApp('/repositories/maven-releases/browse', { user: 'admin' })
    expect(await screen.findByText('Nothing selected')).toBeInTheDocument()
  })

  it('filters every folder by a name prefix kept in the address bar', async () => {
    const user = userEvent.setup()
    const { router } = renderApp('/repositories/maven-releases/browse', { user: 'admin' })
    await tree()
    await screen.findByRole('treeitem', { name: /com/ })
    await user.type(screen.getByLabelText('Name starts with'), 'zzz')
    await waitFor(() => expect(router.state.location.search).toMatchObject({ q: 'zzz' }))
    expect(
      await screen.findByText('No entry at the top level starts with "zzz".'),
    ).toBeInTheDocument()
  })

  it('names the member that serves each entry of a group', async () => {
    renderApp('/repositories/maven-group/browse', { user: 'admin' })
    await screen.findByRole('tree', { name: 'Content of maven-group' })
    const com = await screen.findByRole('treeitem', { name: /com/ })
    expect(within(com).getByText('Served by')).toBeInTheDocument()
    expect(within(com).getByText('maven-releases')).toBeInTheDocument()
  })

  it('explains that an image repository has no file tree and links to its components', async () => {
    renderApp('/repositories/docker-releases/browse', { user: 'admin' })
    expect(await screen.findByText('This repository has no file tree')).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'Open the component list' })).toHaveAttribute(
      'href',
      '/repositories/docker-releases/components',
    )
  })

  it('shows how to publish when the repository is empty', async () => {
    renderApp('/repositories/team-005-raw/browse', { user: 'admin' })
    expect(await screen.findByText('This repository is empty')).toBeInTheDocument()
    expect(await screen.findByRole('region', { name: 'Client setup' })).toBeInTheDocument()
  })

  it('shows the error with code and request id when the folder cannot be read', async () => {
    renderApp('/repositories/maven-releases/browse', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/repositories/:name/tree', () =>
          HttpResponse.json(
            {
              error: {
                code: 'blob_store_unavailable',
                message: 'The blob store is not reachable',
                request_id: 'req-tree',
              },
            },
            { status: 503, headers: { 'X-Request-Id': 'req-tree', 'Retry-After': '30' } },
          ),
        ),
      ],
    })
    expect(await screen.findByText('The content could not be loaded')).toBeInTheDocument()
    expect(screen.getByText('blob_store_unavailable')).toBeInTheDocument()
    expect(screen.getByText('req-tree')).toBeInTheDocument()
  })

  it('adds a "load more" row to a folder with another page and loads it on activation', async () => {
    const user = userEvent.setup()
    const entry = (name: string, kind: 'folder' | 'asset') => ({
      name,
      path: `/${name}${kind === 'folder' ? '/' : ''}`,
      kind,
      asset_id: kind === 'asset' ? `0192f6c4-5a1e-7000-8000-a00000000${name.length}` : null,
      component_id: null,
      size_bytes: kind === 'asset' ? 10 : null,
      content_type: null,
      updated_at: null,
      child_count: null,
      source_repository: null,
    })
    renderApp('/repositories/maven-releases/browse', {
      user: 'admin',
      overrides: [
        http.get('/api/v1/repositories/:name/tree', ({ request }) => {
          const url = new URL(request.url)
          const after = url.searchParams.get('after')
          return HttpResponse.json(
            after === null
              ? {
                  path: '/',
                  items: [entry('one.txt', 'asset'), entry('two.txt', 'asset')],
                  next: 'page-2',
                }
              : { path: '/', items: [entry('three.txt', 'asset')], next: null },
          )
        }),
      ],
    })
    await screen.findByRole('treeitem', { name: /one\.txt/ })
    await user.click(await screen.findByRole('treeitem', { name: 'Load more (2 entries loaded)' }))
    expect(await screen.findByRole('treeitem', { name: /three\.txt/ })).toBeInTheDocument()
    expect(screen.queryByRole('treeitem', { name: /Load more/ })).not.toBeInTheDocument()
  })

  it('tells the user when the selected file was removed meanwhile and refreshes the tree', async () => {
    const { router } = renderApp(
      '/repositories/maven-releases/browse?path=/com/example/nope/gone.jar',
      {
        user: 'admin',
      },
    )
    expect(
      await screen.findByText('That file was removed meanwhile. The tree was refreshed.'),
    ).toBeInTheDocument()
    await waitFor(() => expect(router.state.location.search.path).toBeUndefined())
    expect(await screen.findByText('Nothing selected')).toBeInTheDocument()
  })

  it('deletes the selected file after a confirmation that names it, and refreshes', async () => {
    const user = userEvent.setup()
    const asset = component().assets[0]!
    const { router } = renderApp(`/repositories/maven-releases/browse?path=/${asset.path}`, {
      user: 'admin',
    })
    const pane = within(await screen.findByRole('region', { name: 'Details of the selection' }))
    await user.click(await pane.findByRole('button', { name: 'Delete' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(dialog).toHaveTextContent(`Delete file ${asset.path}?`)
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }))
    await waitFor(() => expect(router.state.location.search.path).toBeUndefined())
    expect(await screen.findByText(`Deleted ${asset.path}.`)).toBeInTheDocument()
    expect(component().assets.some((a) => a.id === asset.id)).toBe(false)
  })

  it('must_not_offer_delete_in_the_pane_to_a_reader', async () => {
    const asset = component().assets[0]!
    const { queryClient } = renderApp(`/repositories/maven-releases/browse?path=/${asset.path}`, {
      user: 'alice',
    })
    const pane = within(await screen.findByRole('region', { name: 'Details of the selection' }))
    await pane.findByText(asset.path)
    await waitFor(() =>
      expect(queryClient.getQueryData(['repositories', 'detail', 'maven-releases'])).toBeDefined(),
    )
    expect(pane.queryByRole('button', { name: 'Delete' })).not.toBeInTheDocument()
    expect(pane.getByRole('link', { name: 'Download' })).toBeInTheDocument()
  })

  it('says "not found or hidden" for a repository that is not there', async () => {
    renderApp('/repositories/no-such/browse', { user: 'admin' })
    expect(
      await screen.findByRole('heading', { name: 'Not found or hidden from you' }),
    ).toBeInTheDocument()
  })
})

describe('tree node helpers', () => {
  it('reveals the folders above a file, and a folder itself', () => {
    expect(revealIdsFor(undefined)).toEqual(['/'])
    expect(revealIdsFor('/')).toEqual(['/'])
    expect(revealIdsFor('/a/b/c.jar')).toEqual(['/', '/a/', '/a/b/'])
    expect(revealIdsFor('/a/b/')).toEqual(['/', '/a/', '/a/b/'])
  })

  it('normalises a path from the address bar', () => {
    expect(cleanPath(undefined)).toBeUndefined()
    expect(cleanPath('')).toBeUndefined()
    expect(cleanPath('a/b')).toBe('/a/b')
    expect(cleanPath('/a')).toBe('/a')
  })

  it('adds one "load more" row only when the server has another page', () => {
    const page = (next: string | null) => ({ path: '/', items: [], next })
    expect(nodesOf(page(null), '/')).toEqual([])
    const rows = nodesOf(page('tok'), '/dir/', 200)
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ more: true, hasChildren: false })
    expect(rows[0]?.label).toBe('Load more (200 entries loaded)')
  })
})
