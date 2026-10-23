import '@/lib/zod-setup'
import { useState } from 'react'
import { z } from 'zod'
import { ApiError } from '@/api/errors'
import {
  applyApiFieldErrors,
  CheckboxField,
  Form,
  SelectField,
  SwitchField,
  TextareaField,
  TextField,
  useZodForm,
} from '@/components/form/form'
import { UnsavedChangesGuard } from '@/components/form/unsaved-changes-guard'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { toast } from '@/lib/toast-store'
import { DemoSection } from '../demo-section'

const schema = z.object({
  name: z
    .string()
    .min(1, 'Enter a name.')
    .regex(
      /^[A-Za-z0-9][A-Za-z0-9._-]*$/,
      'Use letters, digits, dot, dash or underscore, starting with a letter or digit.',
    ),
  remote_url: z
    .string()
    .min(1, 'Enter the remote URL.')
    .url('Enter a full URL, for example https://registry.npmjs.org.'),
  description: z.string().max(200, 'At most 200 characters.'),
  write_policy: z.enum(['allow', 'allow_once', 'deny']),
  online: z.boolean(),
  allow_anonymous: z.boolean(),
})
type Values = z.infer<typeof schema>

/** Stands in for the server: the name "taken" is refused with a field error, like a real 400. */
function fakeCreate(values: Values): Promise<void> {
  return new Promise((resolve, reject) => {
    setTimeout(() => {
      if (values.name === 'taken') {
        reject(
          new ApiError({
            code: 'invalid_input',
            message: 'A repository with this name exists',
            status: 400,
            requestId: '0192f6c4-5a1e-7000-8000-00000000c0de',
            details: {
              fields: [
                { field: '/name', code: 'already_exists', message: 'This name is already used.' },
              ],
            },
          }),
        )
      } else resolve()
    }, 400)
  })
}

export function FormSection() {
  const [done, setDone] = useState(false)
  const form = useZodForm(schema, {
    defaultValues: {
      name: '',
      remote_url: '',
      description: '',
      write_policy: 'allow_once',
      online: true,
      allow_anonymous: false,
    },
  })

  async function onSubmit(values: Values): Promise<void> {
    setDone(false)
    try {
      await fakeCreate(values)
      toast.success('Repository created', { description: values.name })
      // Clear the dirty state first, or the unsaved changes guard blocks what follows the save.
      form.reset(values)
      setDone(true)
    } catch (error) {
      if (!applyApiFieldErrors(form, error))
        toast.apiError(error, 'Could not create the repository')
    }
  }

  return (
    <DemoSection
      id="forms"
      title="Forms"
      description='Visible labels, helper text, inline errors, required markers. Try the name "taken" for a server field error.'
    >
      <Form form={form} onSubmit={onSubmit} aria-label="Create repository" className="max-w-lg">
        <TextField<Values>
          name="name"
          label="Name"
          helper="Cannot be changed after creation."
          required
          autoComplete="off"
        />
        <TextField<Values>
          name="remote_url"
          label="Remote URL"
          required
          inputMode="url"
          placeholder="https://registry.npmjs.org"
        />
        <TextareaField<Values> name="description" label="Description" />
        <SelectField<Values>
          name="write_policy"
          label="Write policy"
          helper="Controls whether published files can be replaced."
        >
          <option value="allow">Allow redeploy</option>
          <option value="allow_once">Allow once</option>
          <option value="deny">Read only</option>
        </SelectField>
        <SwitchField<Values>
          name="online"
          label="Online"
          helper="An offline repository answers 404 to clients."
        />
        <CheckboxField<Values> name="allow_anonymous" label="Allow anonymous read" />
        <div className="flex items-center gap-2">
          <Button type="submit" loading={form.formState.isSubmitting}>
            Create repository
          </Button>
          <Button type="button" variant="outline" disabled>
            Delete
          </Button>
          <span className="text-xs text-muted-foreground">
            Delete is disabled: it needs the nx-admin role.
          </span>
        </div>
        {done ? <Alert variant="success">Saved. The form is clean again.</Alert> : null}
      </Form>
      <UnsavedChangesGuard when={form.formState.isDirty} />
    </DemoSection>
  )
}
