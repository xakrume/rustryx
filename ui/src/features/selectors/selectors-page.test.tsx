import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { http, HttpResponse } from 'msw'
import { beforeEach, describe, expect, it } from 'vitest'
import { commit, identity, resetIdentity } from '@/features/roles/mock-store'
import { expectNoA11yViolations, renderApp } from '@/test/utils'
import '@/features/roles/test-timeouts'

beforeEach(() => resetIdentity())

const inMain = async () => within(await screen.findByRole('main'))
const SELECTOR = (name: string) => identity().selectors.find((s) => s.name === name)
const SLOW = { timeout: 4000 }

async function fillExpression(dialog: HTMLElement, text: string): Promise<HTMLElement> {
  const user = userEvent.setup()
  const box = within(dialog).getByLabelText(/^Expression/)
  await user.click(box)
  await user.paste(text)
  return box
}

describe('content selectors page', () => {
  it('lists selectors with their expression and the privileges that use them', async () => {
    renderApp('/admin/content-selectors', { user: 'admin' })
    const page = await inMain()
    expect(await page.findByRole('button', { name: 'internal-libs' })).toBeInTheDocument()
    expect(page.getByText('internal-libs-read')).toBeInTheDocument()
    expect(page.getAllByText('Not used').length).toBeGreaterThan(0)
    await expectNoA11yViolations(screen.getByRole('main'))
  })

  it('must_show_the_forbidden_panel_to_a_user_who_is_not_an_administrator', async () => {
    renderApp('/admin/content-selectors', { user: 'alice' })
    expect(
      await screen.findByRole('heading', { name: 'You do not have access' }),
    ).toBeInTheDocument()
  })

  it('checks the expression as the user types and creates the selector', async () => {
    const user = userEvent.setup()
    renderApp('/admin/content-selectors?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Create content selector' })
    await user.type(within(dialog).getByLabelText(/^Name/), 'npm-only')
    await fillExpression(dialog, 'format == "npm"')
    expect(
      await within(dialog).findByText('The expression is valid.', undefined, SLOW),
    ).toBeInTheDocument()
    await user.click(within(dialog).getByRole('button', { name: 'Create selector' }))
    expect(await screen.findByText('Content selector npm-only created')).toBeInTheDocument()
    expect(SELECTOR('npm-only')?.expression).toBe('format == "npm"')
  })

  it('must_show_compile_errors_with_their_position_and_not_save_an_expression_that_does_not_compile', async () => {
    const user = userEvent.setup()
    renderApp('/admin/content-selectors?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Create content selector' })
    await user.type(within(dialog).getByLabelText(/^Name/), 'broken')
    const box = await fillExpression(dialog, 'format ==')
    expect(
      await within(dialog).findByText('1 problem in the expression', undefined, SLOW),
    ).toBeInTheDocument()
    const list = within(dialog).getByRole('list', { name: 'Problems in the expression' })
    expect(within(list).getByText('Syntax')).toBeInTheDocument()
    expect(within(list).getByText('Line 1, column 10')).toBeInTheDocument()
    expect(box).toHaveAttribute('aria-invalid', 'true')
    await user.click(
      within(dialog).getByRole('button', { name: /Go to the position of this problem/ }),
    )
    expect(box).toHaveFocus()
    expect((box as HTMLTextAreaElement).selectionStart).toBe(9)
    await user.click(within(dialog).getByRole('button', { name: 'Create selector' }))
    expect(SELECTOR('broken')).toBeUndefined()
    expect(box).toHaveFocus()
    await expectNoA11yViolations(dialog)
  })

  it('compiles the saved text again before saving, even before the live check answered', async () => {
    const user = userEvent.setup()
    renderApp('/admin/content-selectors?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Create content selector' })
    await user.type(within(dialog).getByLabelText(/^Name/), 'fast')
    await fillExpression(dialog, 'path ==')
    await user.click(within(dialog).getByRole('button', { name: 'Create selector' }))
    expect(await within(dialog).findByText('1 problem in the expression')).toBeInTheDocument()
    expect(SELECTOR('fast')).toBeUndefined()
  })

  it('shows compile errors the server reports on save at the expression', async () => {
    const user = userEvent.setup()
    renderApp('/admin/content-selectors?new=1', {
      user: 'admin',
      overrides: [
        http.post('/api/v1/content-selectors', () =>
          HttpResponse.json(
            {
              error: {
                code: 'invalid_selector',
                message: 'bad',
                details: {
                  compile_errors: [{ kind: 'regex', message: 'regex error: nope', offset: 4 }],
                },
              },
            },
            { status: 400 },
          ),
        ),
      ],
    })
    const dialog = await screen.findByRole('dialog', { name: 'Create content selector' })
    await user.type(within(dialog).getByLabelText(/^Name/), 'late')
    await fillExpression(dialog, 'format == "npm"')
    await within(dialog).findByText('The expression is valid.', undefined, SLOW)
    await user.click(within(dialog).getByRole('button', { name: 'Create selector' }))
    expect(await within(dialog).findByText('regex error: nope')).toBeInTheDocument()
    expect(within(dialog).getByText('Regex')).toBeInTheDocument()
  })

  it('validates the name and the expression are not empty', async () => {
    const user = userEvent.setup()
    renderApp('/admin/content-selectors?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Create content selector' })
    await user.click(within(dialog).getByRole('button', { name: 'Create selector' }))
    expect(
      await within(dialog).findByText('Enter a name of 1 to 128 characters.'),
    ).toBeInTheDocument()
    expect(within(dialog).getByText('Enter an expression.')).toBeInTheDocument()
  })

  it('maps a taken name from the server to the field', async () => {
    const user = userEvent.setup()
    renderApp('/admin/content-selectors?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Create content selector' })
    await user.type(within(dialog).getByLabelText(/^Name/), 'internal-libs')
    await fillExpression(dialog, 'format == "npm"')
    await within(dialog).findByText('The expression is valid.', undefined, SLOW)
    await user.click(within(dialog).getByRole('button', { name: 'Create selector' }))
    expect(
      await within(dialog).findByText('A content selector with this name exists'),
    ).toBeInTheDocument()
  })

  it('tests the expression against samples and explains an unknown verdict', async () => {
    const user = userEvent.setup()
    renderApp('/admin/content-selectors?edit=0192f6c4-5a1e-7000-8000-000000000302', {
      user: 'admin',
    })
    const dialog = await screen.findByRole('dialog', { name: /Edit content selector docker-prod/ })
    await user.click(within(dialog).getByRole('button', { name: 'Run test' }))
    const first = await within(dialog).findByLabelText('Result for sample 1')
    expect(within(first).getByText('No match')).toBeInTheDocument()
    expect(within(dialog).getByText('0 of 1 sample matches')).toBeInTheDocument()
    await user.selectOptions(within(dialog).getAllByLabelText('Format')[0]!, 'docker')
    await user.clear(within(dialog).getByLabelText('Coordinate name'))
    await user.click(within(dialog).getByRole('button', { name: 'Run test' }))
    await waitFor(() =>
      expect(
        within(within(dialog).getByLabelText('Result for sample 1')).getByText('Unknown'),
      ).toBeInTheDocument(),
    )
    expect(within(dialog).getByText(/no coordinate\.name/)).toBeInTheDocument()
  })

  it('adds and removes samples, up to the limit of the contract', async () => {
    const user = userEvent.setup()
    renderApp('/admin/content-selectors?edit=0192f6c4-5a1e-7000-8000-000000000300', {
      user: 'admin',
    })
    const dialog = await screen.findByRole('dialog', {
      name: /Edit content selector internal-libs/,
    })
    await user.click(within(dialog).getByRole('button', { name: 'Add sample' }))
    expect(within(dialog).getByText('Sample 2')).toBeInTheDocument()
    await user.click(within(dialog).getByRole('button', { name: 'Remove sample 2' }))
    expect(within(dialog).queryByText('Sample 2')).not.toBeInTheDocument()
    expect(within(dialog).queryByRole('button', { name: /Remove sample/ })).not.toBeInTheDocument()
  })

  it('previews the expression on a repository', async () => {
    const user = userEvent.setup()
    renderApp('/admin/content-selectors?edit=0192f6c4-5a1e-7000-8000-000000000300', {
      user: 'admin',
    })
    const dialog = await screen.findByRole('dialog', {
      name: /Edit content selector internal-libs/,
    })
    await within(dialog).findByRole('option', { name: 'maven-releases' })
    await user.selectOptions(
      within(dialog).getByLabelText('Repository', { selector: '#preview-repository' }),
      'maven-releases',
    )
    await user.click(within(dialog).getByRole('button', { name: 'Preview' }))
    expect(await within(dialog).findByText('3 matching assets')).toBeInTheDocument()
    expect(within(dialog).getByText('com/example/lib/1.0/lib-1.0.jar')).toBeInTheDocument()
  })

  it('shows the syntax help of the engine', async () => {
    const user = userEvent.setup()
    renderApp('/admin/content-selectors?new=1', { user: 'admin' })
    const dialog = await screen.findByRole('dialog', { name: 'Create content selector' })
    await user.click(within(dialog).getByText('Syntax help'))
    expect(await within(dialog).findByText('coordinate.group')).toBeInTheDocument()
    expect(within(dialog).getByText('Expression size (bytes)')).toBeInTheDocument()
  })

  it('must_show_the_conflict_dialog_when_the_selector_changed_after_it_was_opened', async () => {
    const user = userEvent.setup()
    renderApp('/admin/content-selectors?edit=0192f6c4-5a1e-7000-8000-000000000301', {
      user: 'admin',
    })
    const dialog = await screen.findByRole('dialog', { name: /Edit content selector no-snapshots/ })
    await user.type(within(dialog).getByLabelText('Description'), ' mine')
    commit({
      selectors: identity().selectors.map((s) =>
        s.name === 'no-snapshots' ? { ...s, description: 'changed elsewhere' } : s,
      ),
    })
    await user.click(within(dialog).getByRole('button', { name: 'Save selector' }))
    const conflict = await screen.findByRole('alertdialog', { name: 'Changed by someone else' })
    await user.click(within(conflict).getByRole('button', { name: 'Overwrite with my changes' }))
    expect(await screen.findByText('Content selector no-snapshots saved')).toBeInTheDocument()
    expect(SELECTOR('no-snapshots')?.description).toContain('mine')
  })

  it('explains why a selector in use cannot be deleted instead of offering a delete that fails', async () => {
    const user = userEvent.setup()
    renderApp('/admin/content-selectors', { user: 'admin' })
    await user.click(
      await (
        await inMain()
      ).findByRole('button', { name: 'Delete content selector internal-libs' }),
    )
    const dialog = await screen.findByRole('alertdialog', {
      name: 'Cannot delete content selector internal-libs',
    })
    expect(within(dialog).getByRole('link', { name: 'internal-libs-read' })).toBeInTheDocument()
    expect(within(dialog).queryByRole('button', { name: /^Delete/ })).not.toBeInTheDocument()
    await user.click(within(dialog).getAllByRole('button', { name: 'Close' }).at(-1)!)
    expect(SELECTOR('internal-libs')).toBeDefined()
  })

  it('deletes an unused selector after a confirmation that names it', async () => {
    const user = userEvent.setup()
    renderApp('/admin/content-selectors', { user: 'admin' })
    await user.click(
      await (await inMain()).findByRole('button', { name: 'Delete content selector docker-prod' }),
    )
    const dialog = await screen.findByRole('alertdialog', {
      name: /Delete content selector\s+docker-prod\s*\?/,
    })
    await user.click(within(dialog).getByRole('button', { name: 'Delete content selector' }))
    expect(await screen.findByText('Content selector docker-prod deleted')).toBeInTheDocument()
    expect(SELECTOR('docker-prod')).toBeUndefined()
  })
})
