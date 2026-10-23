import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { z } from 'zod'
import { describe, expect, it, vi } from 'vitest'
import { ApiError } from '@/api/errors'
import { expectNoA11yViolations, renderWithProviders } from '@/test/utils'
import { Button } from '@/components/ui/button'
import { FormField } from './form-field'
import {
  applyApiFieldErrors,
  CheckboxField,
  Form,
  pointerToName,
  SelectField,
  SwitchField,
  TextareaField,
  TextField,
  useZodForm,
} from './form'

const schema = z.object({
  name: z.string().min(1, 'Enter a name.'),
  remote_url: z.string().url('Enter a full URL.'),
  note: z.string(),
  policy: z.enum(['allow', 'deny']),
  online: z.boolean(),
  anonymous: z.boolean(),
})
type Values = z.infer<typeof schema>

function Example({ onSubmit }: { onSubmit: (v: Values) => void | Promise<void> }) {
  const form = useZodForm(schema, {
    defaultValues: {
      name: '',
      remote_url: '',
      note: '',
      policy: 'allow',
      online: true,
      anonymous: false,
    },
  })
  return (
    <>
      <Form form={form} onSubmit={onSubmit} aria-label="Repository">
        <TextField<Values> name="name" label="Name" helper="Cannot change later." required />
        <TextField<Values> name="remote_url" label="Remote URL" required />
        <TextareaField<Values> name="note" label="Note" />
        <SelectField<Values> name="policy" label="Policy">
          <option value="allow">Allow</option>
          <option value="deny">Deny</option>
        </SelectField>
        <SwitchField<Values> name="online" label="Online" />
        <CheckboxField<Values> name="anonymous" label="Anonymous read" />
        <Button type="submit">Save</Button>
      </Form>
      <button
        type="button"
        onClick={() =>
          applyApiFieldErrors(
            form,
            new ApiError({
              code: 'invalid_input',
              message: 'm',
              status: 400,
              details: {
                fields: [
                  {
                    field: '/remote_url',
                    code: 'upstream_url_rejected',
                    message: 'The address is blocked.',
                  },
                  { field: '/name', code: 'already_exists', message: 'Name is taken.' },
                ],
              },
            }),
          )
        }
      >
        server errors
      </button>
    </>
  )
}

describe('form fields', () => {
  it('has a visible label, a required marker for assistive tech, helper text and no violations', async () => {
    const { container } = renderWithProviders(<Example onSubmit={vi.fn()} />)
    const name = screen.getByLabelText(/^Name/)
    expect(name).toHaveAttribute('aria-required', 'true')
    expect(name).toHaveAccessibleDescription('Cannot change later.')
    expect(screen.getAllByText('(required)')[0]).toHaveClass('sr-only')
    await expectNoA11yViolations(container)
  })

  it('shows the error next to the field on blur, wired with aria-invalid and aria-describedby', async () => {
    const user = userEvent.setup()
    renderWithProviders(<Example onSubmit={vi.fn()} />)
    const name = screen.getByLabelText(/^Name/)
    await user.click(name)
    await user.tab()
    expect(await screen.findByText('Enter a name.')).toBeInTheDocument()
    expect(name).toHaveAttribute('aria-invalid', 'true')
    expect(name).toHaveAccessibleDescription(expect.stringContaining('Enter a name.'))
    // Fixing it clears the error at once.
    await user.type(name, 'maven')
    await waitFor(() => expect(screen.queryByText('Enter a name.')).not.toBeInTheDocument())
  })

  it('does not submit an invalid form and focuses the first invalid field', async () => {
    const user = userEvent.setup()
    const onSubmit = vi.fn()
    renderWithProviders(<Example onSubmit={onSubmit} />)
    await user.click(screen.getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(screen.getByLabelText(/^Name/)).toHaveFocus())
    expect(onSubmit).not.toHaveBeenCalled()
    expect(screen.getByText('Enter a full URL.')).toBeInTheDocument()
  })

  it('submits the typed, validated values', async () => {
    const user = userEvent.setup()
    const onSubmit = vi.fn()
    renderWithProviders(<Example onSubmit={onSubmit} />)
    await user.type(screen.getByLabelText(/^Name/), 'npm-proxy')
    await user.type(screen.getByLabelText(/^Remote URL/), 'https://registry.npmjs.org')
    await user.selectOptions(screen.getByLabelText('Policy'), 'deny')
    await user.click(screen.getByRole('checkbox', { name: 'Anonymous read' }))
    await user.click(screen.getByRole('switch', { name: 'Online' }))
    await user.click(screen.getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(onSubmit).toHaveBeenCalled())
    expect(onSubmit.mock.calls[0]?.[0]).toMatchObject({
      name: 'npm-proxy',
      remote_url: 'https://registry.npmjs.org',
      policy: 'deny',
      anonymous: true,
      online: false,
    })
  })

  it('maps API field errors onto the form and focuses the first one', async () => {
    const user = userEvent.setup()
    renderWithProviders(<Example onSubmit={vi.fn()} />)
    await user.click(screen.getByRole('button', { name: 'server errors' }))
    expect(await screen.findByText('The address is blocked.')).toBeInTheDocument()
    expect(screen.getByText('Name is taken.')).toBeInTheDocument()
    await waitFor(() => expect(screen.getByLabelText(/^Remote URL/)).toHaveFocus())
  })
})

describe('FormField', () => {
  it('works with any control through the render prop', async () => {
    const { container } = renderWithProviders(
      <FormField label="Custom" helper="Hint" error="Broken" required id="custom">
        {(control) => <input {...control} />}
      </FormField>,
    )
    const input = screen.getByLabelText(/^Custom/)
    expect(input).toHaveAttribute('id', 'custom')
    expect(input).toHaveAttribute('aria-invalid', 'true')
    expect(input).toHaveAccessibleDescription('Hint Broken')
    await expectNoA11yViolations(container)
  })

  it('must_not_mark_a_valid_field_as_invalid', () => {
    renderWithProviders(<FormField label="Fine">{(control) => <input {...control} />}</FormField>)
    const input = screen.getByLabelText('Fine')
    expect(input).not.toHaveAttribute('aria-invalid')
    expect(input).not.toHaveAttribute('aria-describedby')
  })
})

describe('pointerToName and applyApiFieldErrors', () => {
  it('converts JSON pointers to form paths', () => {
    expect(pointerToName('/name')).toBe('name')
    expect(pointerToName('/proxy/remote_url')).toBe('proxy.remote_url')
    expect(pointerToName('/members/0/name')).toBe('members.0.name')
    expect(pointerToName('/a~1b/c~0d')).toBe('a/b.c~d')
    expect(pointerToName('limit')).toBe('limit')
  })

  it('returns false for an error without field details so the caller can toast instead', () => {
    const form = { setError: vi.fn() } as never
    expect(
      applyApiFieldErrors(
        form,
        new ApiError({ code: 'internal_error', message: 'm', status: 500 }),
      ),
    ).toBe(false)
    expect(applyApiFieldErrors(form, new Error('x'))).toBe(false)
  })
})
