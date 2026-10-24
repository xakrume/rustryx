import { configure, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'
import { resetRepositories } from '@/features/repositories/mock-state'
import { repositories } from '@/mocks/store'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import { AttributeList } from './attribute-list'
import { ownComponents, resetContent } from './mock-content'

// The first renders of a file are slow when many test files run at once.
configure({ asyncUtilTimeout: 10_000 })

beforeEach(() => {
  resetRepositories()
  resetContent()
})

const first = (repository: string) =>
  ownComponents(repositories.find((r) => r.name === repository)!)[0]!

describe('component detail', () => {
  it('shows the package version, its attributes and its files with checksums to copy', async () => {
    const component = first('maven-releases')
    const { container } = renderApp(`/components/${component.id}`, { user: 'admin' })
    expect(
      await screen.findByRole('heading', { level: 1, name: new RegExp(component.name) }),
    ).toBeInTheDocument()
    const main = within(screen.getByRole('main'))
    expect(main.getByText('packaging')).toBeInTheDocument()
    expect(main.getByText('jar', { selector: 'dd' })).toBeInTheDocument()
    const table = main.getByRole('region', { name: 'Files' })
    expect(within(table).getAllByRole('row')).toHaveLength(component.assets.length + 1)
    expect(within(table).getAllByRole('button', { name: 'Copy SHA-256 checksum' })).toHaveLength(
      component.assets.length,
    )
    expect(main.getByRole('link', { name: 'Show in tree' })).toHaveAttribute(
      'href',
      expect.stringContaining('/repositories/maven-releases/browse'),
    )
    await expectNoA11yViolations(container)
  })

  it('deletes the component only after a confirmation that names it, then leaves the page', async () => {
    const user = userEvent.setup()
    const component = first('maven-releases')
    renderApp(`/components/${component.id}`, { user: 'admin' })
    await screen.findByRole('heading', { level: 1 })
    await user.click(await screen.findByRole('button', { name: 'Delete' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(dialog).toHaveTextContent(
      `Delete component ${component.group}:${component.name} ${component.version}?`,
    )
    expect(dialog).toHaveTextContent(`its ${component.assets.length} files`)
    // Nothing is deleted until the confirmation.
    expect(ownComponents(repositories.find((r) => r.name === 'maven-releases')!)).toContain(
      component,
    )
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }))
    expect(
      await screen.findByRole('heading', { level: 1, name: 'maven-releases' }),
    ).toBeInTheDocument()
    expect(ownComponents(repositories.find((r) => r.name === 'maven-releases')!)).not.toContain(
      component,
    )
  })

  it('must_not_offer_delete_to_someone_who_may_not_delete', async () => {
    const component = first('maven-releases')
    const { queryClient } = renderApp(`/components/${component.id}`, { user: 'alice' })
    await screen.findByRole('heading', { level: 1 })
    // Wait until the repository (and with it the caller's rights on it) is known.
    await waitFor(() =>
      expect(queryClient.getQueryData(['repositories', 'detail', 'maven-releases'])).toBeDefined(),
    )
    expect(screen.queryByRole('button', { name: 'Delete' })).not.toBeInTheDocument()
  })

  it('keeps the dialog open and shows the code when the server refuses the delete', async () => {
    const user = userEvent.setup()
    const component = first('maven-releases')
    renderApp(`/components/${component.id}`, {
      user: 'admin',
      overrides: [
        http.delete('/api/v1/components/:id', () =>
          HttpResponse.json(
            {
              error: {
                code: 'immutable_tag',
                message: 'An immutability rule protects it',
                request_id: 'req-imm',
              },
            },
            { status: 409, headers: { 'X-Request-Id': 'req-imm' } },
          ),
        ),
      ],
    })
    await screen.findByRole('heading', { level: 1 })
    await user.click(await screen.findByRole('button', { name: 'Delete' }))
    const dialog = await screen.findByRole('alertdialog')
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }))
    expect(await within(dialog).findByText('immutable_tag')).toBeInTheDocument()
    expect(within(dialog).getByText('req-imm')).toBeInTheDocument()
  })

  it('treats a delete that finds nothing as done', async () => {
    const user = userEvent.setup()
    const component = first('maven-releases')
    renderApp(`/components/${component.id}`, {
      user: 'admin',
      overrides: [
        http.delete('/api/v1/components/:id', () =>
          HttpResponse.json(
            { error: { code: 'component_not_found', message: 'Gone', request_id: 'r' } },
            { status: 404 },
          ),
        ),
      ],
    })
    await screen.findByRole('heading', { level: 1 })
    await user.click(await screen.findByRole('button', { name: 'Delete' }))
    await user.click(
      within(await screen.findByRole('alertdialog')).getByRole('button', { name: 'Delete' }),
    )
    expect(
      await screen.findByRole('heading', { level: 1, name: 'maven-releases' }),
    ).toBeInTheDocument()
  })

  it('says "not found or hidden" for a component that is not there', async () => {
    renderApp('/components/0192f6c4-5a1e-7000-8000-c0ffffffffff', { user: 'admin' })
    expect(
      await screen.findByRole('heading', { name: 'Not found or hidden from you' }),
    ).toBeInTheDocument()
    expect(screen.getByText('component_not_found')).toBeInTheDocument()
  })

  it('shows tags and digest of an image from its attributes', async () => {
    const component = first('docker-releases')
    renderApp(`/components/${component.id}`, { user: 'admin' })
    await screen.findByRole('heading', { level: 1, name: 'acme/api' })
    expect(screen.getByText(String(component.attributes.digest))).toBeInTheDocument()
    // An image has no path tree to show it in.
    expect(screen.queryByRole('link', { name: 'Show in tree' })).not.toBeInTheDocument()
  })
})

describe('asset detail', () => {
  it('shows the file with all its facts, three checksums to copy and a download link', async () => {
    const asset = first('maven-releases').assets[0]!
    const { container } = renderApp(`/assets/${asset.id}`, { user: 'admin' })
    expect(
      await screen.findByRole('heading', { level: 1, name: asset.path.split('/').pop() }),
    ).toBeInTheDocument()
    const main = within(screen.getByRole('main'))
    expect(main.getByText(asset.path)).toBeInTheDocument()
    expect(main.getByText(asset.sha256)).toBeInTheDocument()
    for (const algorithm of ['SHA-256', 'SHA-1', 'MD5']) {
      expect(main.getByRole('button', { name: `Copy ${algorithm} checksum` })).toBeInTheDocument()
    }
    expect(main.getByRole('link', { name: 'Download' })).toHaveAttribute(
      'href',
      `https://repo.example.org/repository/maven-releases/${asset.path}`,
    )
    expect(main.getByRole('button', { name: /^Copy file URL/ })).toBeInTheDocument()
    expect(main.getByText('Downloads, 30 days')).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('warns that the blob is missing and points an administrator at the tasks', async () => {
    const component = ownComponents(repositories.find((r) => r.name === 'raw-releases')!).find(
      (c) => c.assets.some((a) => a.path.includes('missing')),
    )!
    renderApp(`/assets/${component.assets[0]!.id}`, { user: 'admin' })
    expect(await screen.findByText('The file is missing in the blob store')).toBeInTheDocument()
    expect(screen.getByRole('link', { name: /blob verification/ })).toHaveAttribute(
      'href',
      '/admin/tasks',
    )
  })

  it('deletes a file after a confirmation that names its path', async () => {
    const user = userEvent.setup()
    const component = first('maven-releases')
    const asset = component.assets[0]!
    renderApp(`/assets/${asset.id}`, { user: 'admin' })
    await screen.findByRole('heading', { level: 1 })
    await user.click(await screen.findByRole('button', { name: 'Delete' }))
    const dialog = await screen.findByRole('alertdialog')
    expect(dialog).toHaveTextContent(`Delete file ${asset.path}?`)
    expect(dialog).toHaveTextContent('garbage collection')
    await user.click(within(dialog).getByRole('button', { name: 'Delete' }))
    expect(
      await screen.findByRole('heading', { level: 1, name: 'maven-releases' }),
    ).toBeInTheDocument()
    expect(component.assets.length).toBeGreaterThan(1)
  })

  it('must_not_offer_delete_or_the_blob_key_to_a_reader', async () => {
    const asset = first('maven-releases').assets[0]!
    const { queryClient } = renderApp(`/assets/${asset.id}`, { user: 'alice' })
    await screen.findByRole('heading', { level: 1 })
    await waitFor(() =>
      expect(queryClient.getQueryData(['repositories', 'detail', 'maven-releases'])).toBeDefined(),
    )
    expect(screen.queryByRole('button', { name: 'Delete' })).not.toBeInTheDocument()
    expect(document.body.textContent).not.toContain('content/')
  })

  it('says "not found or hidden" for a file that is not there', async () => {
    renderApp('/assets/0192f6c4-5a1e-7000-8000-a0ffffffffff', { user: 'admin' })
    expect(
      await screen.findByRole('heading', { name: 'Not found or hidden from you' }),
    ).toBeInTheDocument()
  })

  it('refuses to delete from a group repository', async () => {
    const user = userEvent.setup()
    const asset = first('maven-releases').assets[0]!
    renderApp(`/assets/${asset.id}`, {
      user: 'admin',
      overrides: [
        http.delete('/api/v1/assets/:id', () =>
          HttpResponse.json(
            {
              error: {
                code: 'repository_not_hosted',
                message: 'Group repositories have no content',
                request_id: 'r',
              },
            },
            { status: 409 },
          ),
        ),
      ],
    })
    await screen.findByRole('heading', { level: 1 })
    await user.click(await screen.findByRole('button', { name: 'Delete' }))
    await user.click(
      within(await screen.findByRole('alertdialog')).getByRole('button', { name: 'Delete' }),
    )
    expect(await screen.findByText('repository_not_hosted')).toBeInTheDocument()
    await waitFor(() => expect(screen.getByRole('alertdialog')).toBeInTheDocument())
  })
})

describe('attribute list', () => {
  it('shows text, chips for lists and formatted JSON for objects, never markup', () => {
    render(
      <AttributeList
        attributes={{
          packaging: 'jar',
          flag: true,
          nothing: null,
          tags: ['latest', '1.0'],
          nested: { a: { b: 1 } },
          html: '<img src=x onerror=alert(1)>',
        }}
      />,
    )
    expect(screen.getByText('jar')).toBeInTheDocument()
    expect(screen.getByText('true')).toBeInTheDocument()
    expect(screen.getByText('latest')).toBeInTheDocument()
    expect(document.querySelector('pre')?.textContent).toContain('"b": 1')
    expect(document.querySelector('img')).toBeNull()
    expect(screen.getByText('<img src=x onerror=alert(1)>')).toBeInTheDocument()
  })

  it('says when there is nothing to show', () => {
    render(<AttributeList attributes={{}} />)
    expect(screen.getByText('No attributes.')).toBeInTheDocument()
  })
})
