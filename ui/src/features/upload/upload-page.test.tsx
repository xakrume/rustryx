import { act, configure, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { Blob as NodeBlob, File as NodeFile } from 'node:buffer'
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'
import { ownComponents, resetContent } from '@/features/components/mock-content'
import { resetRepositories } from '@/features/repositories/mock-state'
import { repositories } from '@/mocks/store'
import { FakeXhr } from '@/test/fake-xhr'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import { parseFileName } from './mocks'
import { explainUploadError, ProgressPanel } from './upload-status'
import { ApiError } from '@/api/errors'

// The first renders of a file are slow when many test files run at once.
configure({ asyncUtilTimeout: 10_000 })

/**
 * jsdom has its own FormData, Blob and File, and the request that MSW rebuilds from a jsdom
 * FormData has no multipart body (it becomes the text "[object FormData]"). The server mock reads
 * a real multipart body, so these tests build the upload with Node's classes, which is what a
 * browser sends on the wire. Node's FormData is only reachable through a Response.
 */
beforeAll(async () => {
  const nodeFormData = (
    await new Response('', {
      headers: { 'content-type': 'application/x-www-form-urlencoded' },
    }).formData()
  ).constructor
  vi.stubGlobal('FormData', nodeFormData)
  vi.stubGlobal('Blob', NodeBlob)
  // undici builds the parsed file parts with the global File, which must be the same family.
  vi.stubGlobal('File', NodeFile)
})
afterAll(() => vi.unstubAllGlobals())

beforeEach(() => {
  resetRepositories()
  resetContent()
})

const jar = (name = 'api-core-9.9.9.jar') =>
  new NodeFile(['0123456789'], name, { type: 'application/java-archive' }) as unknown as File

async function openForm(
  path = '/upload?repository=maven-releases',
  user: 'admin' | 'alice' = 'admin',
) {
  const result = renderApp(path, { user })
  await screen.findByRole('heading', { name: 'Files' })
  return result
}

const fileInput = () => document.querySelector<HTMLInputElement>('input[type="file"]')!

describe('upload', () => {
  it('builds the form from the upload schema of the chosen repository', async () => {
    const { container } = await openForm()
    expect(screen.getByLabelText(/^Group ID/)).toBeInTheDocument()
    expect(screen.getByLabelText('Packaging')).toHaveValue('jar')
    // A field the server can read from the POM is not required of the user, and says so.
    expect(
      screen.getAllByText('Read from the POM when you leave it empty.').length,
    ).toBeGreaterThan(0)
    expect(screen.getByText(/Accepts \.jar, \.pom, \.war, \.zip/)).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('picks the repository from the list and changes the form with it', async () => {
    const user = userEvent.setup()
    const { router } = renderApp('/upload', { user: 'admin' })
    await user.selectOptions(await screen.findByLabelText('Repository'), 'npm-releases')
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({ repository: 'npm-releases' }),
    )
    expect(await screen.findByLabelText('Dist tag')).toHaveValue('latest')
    expect(screen.queryByLabelText(/^Group ID/)).not.toBeInTheDocument()
  })

  it('uploads a Maven component with per file fields and shows the result with links', async () => {
    const user = userEvent.setup()
    await openForm()
    await user.type(screen.getByLabelText(/^Group ID/), 'org.acme')
    await user.upload(fileInput(), [jar('lib-2.0.jar'), jar('lib-2.0-sources.jar')])
    const list = screen.getByRole('list', { name: 'Selected files' })
    expect(within(list).getAllByRole('listitem')).toHaveLength(2)
    await user.type(within(list).getAllByLabelText(/^Classifier/)[1]!, 'sources')
    await user.click(screen.getByRole('button', { name: 'Upload' }))

    const result = await screen.findByRole('region', { name: 'Upload complete' })
    expect(within(result).getByText(/Published 2 files as/)).toBeInTheDocument()
    expect(within(result).getByRole('link', { name: /org\.acme:lib 2\.0/ })).toBeInTheDocument()
    expect(
      within(result).getByRole('link', { name: 'org/acme/lib/2.0/lib-2.0.jar' }),
    ).toBeInTheDocument()
    const published = ownComponents(repositories.find((r) => r.name === 'maven-releases')!)[0]!
    expect(published).toMatchObject({ group: 'org.acme', name: 'lib', version: '2.0' })
    expect(published.assets).toHaveLength(2)
    await user.click(within(result).getByRole('button', { name: 'Upload another' }))
    expect(await screen.findByRole('heading', { name: 'Files' })).toBeInTheDocument()
    expect(screen.queryByRole('list', { name: 'Selected files' })).not.toBeInTheDocument()
  })

  it('takes dropped files as well as chosen ones, and lets one be removed', async () => {
    const user = userEvent.setup()
    await openForm()
    const zone = fileInput().parentElement!
    fireEvent.dragOver(zone, { dataTransfer: { files: [] } })
    fireEvent.drop(zone, { dataTransfer: { files: [jar('a-1.0.jar'), jar('b-1.0.jar')] } })
    const list = await screen.findByRole('list', { name: 'Selected files' })
    expect(within(list).getAllByRole('listitem')).toHaveLength(2)
    await user.click(screen.getByRole('button', { name: 'Remove a-1.0.jar' }))
    expect(within(list).getAllByRole('listitem')).toHaveLength(1)
  })

  it('reaches the file chooser with the keyboard', async () => {
    const user = userEvent.setup()
    await openForm()
    const input = fileInput()
    // It is visually hidden but a real, focusable control, with the drop area as its label.
    input.focus()
    expect(input).toHaveFocus()
    expect(screen.getByText('Choose files').closest('label')).toHaveAttribute('for', input.id)
    await user.keyboard('{Tab}')
    expect(input).not.toHaveFocus()
  })

  it('checks the form before it uploads: file type, count and missing files', async () => {
    const user = userEvent.setup()
    await openForm()
    await user.click(screen.getByRole('button', { name: 'Upload' }))
    expect(await screen.findByText('Choose at least one file.')).toBeInTheDocument()
    fireEvent.drop(fileInput().parentElement!, {
      dataTransfer: { files: [new NodeFile(['x'], 'notes.txt') as unknown as File] },
    })
    await user.click(screen.getByRole('button', { name: 'Upload' }))
    expect(await screen.findByText(/This file type is not accepted/)).toBeInTheDocument()
  })

  it('replaces the file of a single file format instead of adding to it', async () => {
    const user = userEvent.setup()
    renderApp('/upload?repository=npm-releases', { user: 'admin' })
    await screen.findByLabelText('Dist tag')
    await user.upload(fileInput(), new NodeFile(['x'], 'ui-1.0.0.tgz') as unknown as File)
    await user.upload(fileInput(), new NodeFile(['y'], 'api-2.0.0.tgz') as unknown as File)
    const list = screen.getByRole('list', { name: 'Selected files' })
    expect(within(list).getAllByRole('listitem')).toHaveLength(1)
    expect(within(list).getByText('api-2.0.0.tgz')).toBeInTheDocument()
  })

  it('explains the write policy when a second upload to the same path is refused', async () => {
    const user = userEvent.setup()
    await openForm()
    await user.upload(fileInput(), jar('twice-1.0.jar'))
    await user.click(screen.getByRole('button', { name: 'Upload' }))
    await screen.findByRole('region', { name: 'Upload complete' })
    await user.click(screen.getByRole('button', { name: 'Upload another' }))
    await user.upload(fileInput(), jar('twice-1.0.jar'))
    await user.click(screen.getByRole('button', { name: 'Upload' }))
    expect(await screen.findByText('The repository refused this upload')).toBeInTheDocument()
    expect(screen.getByText(/set to deploy once/)).toBeInTheDocument()
    expect(screen.getByText('write_policy_violation')).toBeInTheDocument()
  })

  it.each([
    ['quota-1.0.jar', 'There is no room left', /Used 9\.9 GiB of 10 GiB/],
    ['digest-1.0.jar', 'The file arrived damaged', /do not match the checksum/],
    ['toolarge-1.0.jar', 'The file is too large', /does not accept a file this size/],
  ])('says in plain words why %s failed', async (name, title, advice) => {
    const user = userEvent.setup()
    await openForm()
    await user.upload(fileInput(), jar(name))
    await user.click(screen.getByRole('button', { name: 'Upload' }))
    expect(await screen.findByText(title)).toBeInTheDocument()
    expect(screen.getByText(advice)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Retry upload' })).toBeInTheDocument()
  })

  it('shows code and request id for a failure it has no special words for', async () => {
    const user = userEvent.setup()
    await openForm()
    await user.upload(fileInput(), jar('unavailable-1.0.jar'))
    await user.click(screen.getByRole('button', { name: 'Upload' }))
    expect(await screen.findByText('The upload failed')).toBeInTheDocument()
    expect(screen.getByText('blob_store_unavailable')).toBeInTheDocument()
    expect(screen.getAllByText(/^0192f6c4-/).length).toBeGreaterThan(0)
  })

  it('retries the same upload after a failure and succeeds', async () => {
    const user = userEvent.setup()
    let calls = 0
    const keys: (string | null)[] = []
    await (async () => {
      renderApp('/upload?repository=maven-releases', {
        user: 'admin',
        overrides: [
          http.post('/api/v1/components', ({ request }) => {
            calls += 1
            keys.push(request.headers.get('idempotency-key'))
            if (calls === 1) {
              return HttpResponse.json(
                { error: { code: 'upstream_error', message: 'try again', request_id: 'r' } },
                { status: 502 },
              )
            }
            return undefined
          }),
        ],
      })
      await screen.findByRole('heading', { name: 'Files' })
    })()
    await user.upload(fileInput(), jar('retry-1.0.jar'))
    await user.click(screen.getByRole('button', { name: 'Upload' }))
    await user.click(await screen.findByRole('button', { name: 'Retry upload' }))
    await screen.findByRole('region', { name: 'Upload complete' })
    // The same upload keeps its idempotency key, so a retry cannot publish twice.
    expect(keys[0]).toBeTruthy()
    expect(keys[1]).toBe(keys[0])
  })

  it('maps a server field error onto the field and focuses it', async () => {
    const user = userEvent.setup()
    renderApp('/upload?repository=maven-releases', {
      user: 'admin',
      overrides: [
        http.post('/api/v1/components', () =>
          HttpResponse.json(
            {
              error: {
                code: 'invalid_input',
                message: 'The request is not valid',
                request_id: 'r',
                details: {
                  fields: [{ field: '/fields/group_id', code: 'x', message: 'Not a Maven group' }],
                },
              },
            },
            { status: 400 },
          ),
        ),
      ],
    })
    await screen.findByRole('heading', { name: 'Files' })
    await user.upload(fileInput(), jar())
    await user.click(screen.getByRole('button', { name: 'Upload' }))
    expect(await screen.findByText('Not a Maven group')).toBeInTheDocument()
    await waitFor(() => expect(screen.getByLabelText(/^Group ID/)).toHaveFocus())
  })

  it('shows progress while the file goes up and cancels on request, publishing nothing', async () => {
    const user = userEvent.setup()
    const real = globalThis.XMLHttpRequest
    FakeXhr.instances = []
    vi.stubGlobal('XMLHttpRequest', FakeXhr)
    try {
      await openForm()
      await user.upload(fileInput(), jar('slow-1.0.jar'))
      await user.click(screen.getByRole('button', { name: 'Upload' }))
      const progress = await screen.findByRole('region', { name: 'Uploading' })
      act(() =>
        FakeXhr.instances[0]!.upload.onprogress?.({ lengthComputable: true, loaded: 5, total: 10 }),
      )
      expect(
        await within(progress).findByRole('progressbar', { name: 'Overall progress' }),
      ).toHaveAttribute('aria-valuenow', '50')
      expect(
        within(progress).getByRole('progressbar', { name: 'Progress of slow-1.0.jar' }),
      ).toHaveAttribute('aria-valuenow', '50')
      // While it runs the form cannot be edited, and Cancel is the way out.
      expect(screen.getByLabelText(/^Group ID/)).toBeDisabled()
      await user.click(within(progress).getByRole('button', { name: 'Cancel upload' }))
      expect(
        await screen.findByText(/The upload was cancelled\. Nothing was published/),
      ).toBeInTheDocument()
      expect(screen.getByRole('list', { name: 'Selected files' })).toBeInTheDocument()
      expect(screen.getByLabelText(/^Group ID/)).toBeEnabled()
    } finally {
      vi.stubGlobal('XMLHttpRequest', real)
    }
  })

  it('completes the upload the fake network answers and shows the result', async () => {
    const user = userEvent.setup()
    const real = globalThis.XMLHttpRequest
    FakeXhr.instances = []
    vi.stubGlobal('XMLHttpRequest', FakeXhr)
    try {
      await openForm()
      await user.upload(fileInput(), jar('quick-1.0.jar'))
      await user.click(screen.getByRole('button', { name: 'Upload' }))
      await screen.findByRole('region', { name: 'Uploading' })
      const repo = repositories.find((r) => r.name === 'maven-releases')!
      act(() =>
        FakeXhr.instances[0]!.finish(201, {
          component: {
            id: '0192f6c4-5a1e-7000-8000-c00000000001',
            repository_id: repo.id,
            format: 'maven2',
            group_id: 'com.example',
            name: 'quick',
            version: '1.0',
            created_at: repo.created_at,
            updated_at: repo.created_at,
            repository: repo.name,
            asset_count: 1,
            size_bytes: 10,
            attributes_summary: {},
          },
          assets: [],
          replaced: ['com/example/quick/1.0/quick-1.0.jar'],
        }),
      )
      const result = await screen.findByRole('region', { name: 'Upload complete' })
      // A file that replaced an earlier one is named, so an overwrite is never silent.
      expect(within(result).getByText('These files replaced earlier ones')).toBeInTheDocument()
      expect(within(result).getByText('com/example/quick/1.0/quick-1.0.jar')).toBeInTheDocument()
    } finally {
      vi.stubGlobal('XMLHttpRequest', real)
    }
  })

  it('explains that an image repository is published with its own client and shows how', async () => {
    renderApp('/upload?repository=docker-releases', { user: 'admin' })
    expect(await screen.findByText('Docker cannot be uploaded in the browser')).toBeInTheDocument()
    expect(await screen.findByRole('region', { name: 'Client setup' })).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Upload' })).not.toBeInTheDocument()
  })

  it('explains why a proxy cannot take uploads', async () => {
    renderApp('/upload?repository=maven-proxy', { user: 'admin' })
    expect(await screen.findByText('You cannot upload to maven-proxy')).toBeInTheDocument()
    expect(screen.getByText(/Only hosted repositories take uploads/)).toBeInTheDocument()
  })

  it('must_list_only_hosted_repositories_the_caller_may_write_to', async () => {
    renderApp('/upload', { user: 'alice' })
    const select = await screen.findByLabelText('Repository')
    const options = within(select)
      .getAllByRole('option')
      .map((o) => o.textContent)
    expect(options).toContain('maven-releases (Maven)')
    expect(options.some((o) => o?.startsWith('maven-proxy'))).toBe(false)
    expect(options.some((o) => o?.startsWith('maven-group'))).toBe(false)
  })

  it('must_not_open_for_someone_who_may_not_upload', async () => {
    renderApp('/upload', {})
    expect(await screen.findByRole('heading', { name: 'Sign in to Rustryx' })).toBeInTheDocument()
  })

  it('says so when a repository in the address bar does not exist', async () => {
    renderApp('/upload?repository=nope', { user: 'admin' })
    expect(await screen.findByText(/no repository named nope/)).toBeInTheDocument()
  })
})

describe('upload status', () => {
  it('shows progress per file and overall as progress bars', async () => {
    const files = [
      { id: 'a', file: new File([new Uint8Array(100)], 'a.jar') },
      { id: 'b', file: new File([new Uint8Array(100)], 'b.jar') },
    ]
    const { container } = render(
      <ProgressPanel files={files} progress={[1, 0.25]} onCancel={() => undefined} />,
    )
    expect(screen.getByRole('progressbar', { name: 'Overall progress' })).toHaveAttribute(
      'aria-valuenow',
      '63',
    )
    expect(screen.getByRole('progressbar', { name: 'Progress of a.jar' })).toHaveAttribute(
      'aria-valuenow',
      '100',
    )
    expect(screen.getByRole('progressbar', { name: 'Progress of b.jar' })).toHaveAttribute(
      'aria-valuenow',
      '25',
    )
    await expectNoA11yViolations(container)
  })

  it('has words for the failures of the inventory and none for others', () => {
    const error = (code: string, status = 409, details: Record<string, unknown> = {}) =>
      new ApiError({ code, message: 'm', status, details })
    expect(
      explainUploadError(error('write_policy_violation', 409, { policy: 'deny' }))?.advice,
    ).toMatch(/read only/)
    expect(explainUploadError(error('repository_not_hosted'))?.title).toMatch(/not a hosted/)
    expect(explainUploadError(error('network_error', 0))?.title).toMatch(/interrupted/)
    expect(explainUploadError(error('quota_exceeded', 507))?.advice).toMatch(/Used - of -/)
    expect(explainUploadError(error('internal_error', 500))).toBeNull()
    expect(explainUploadError(new Error('x'))).toBeNull()
  })

  it('reads name and version from a file name', () => {
    expect(parseFileName('api-core-1.2.3-sources.jar')).toEqual({
      name: 'api-core',
      version: '1.2.3',
    })
    expect(parseFileName('acme_api-2.0.0.tar.gz')).toEqual({ name: 'acme_api', version: '2.0.0' })
    expect(parseFileName('README.md')).toEqual({ name: 'README', version: '0.0.0' })
  })
})
