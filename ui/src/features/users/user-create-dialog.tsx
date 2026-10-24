import '@/lib/zod-setup'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { useState } from 'react'
import { Controller } from 'react-hook-form'
import { z } from 'zod'
import { isApiError } from '@/api/errors'
import { applyApiFieldErrors, Form, TextField, useZodForm } from '@/components/form/form'
import { UnsavedChangesGuard } from '@/components/form/unsaved-changes-guard'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { rolePickerQuery } from '@/features/roles/api'
import { useFinish } from '@/features/roles/use-finish'
import { MultiPicker, type PickerItem } from '@/features/roles/multi-picker'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { createUser, userKeys } from './api'
import { PasswordInput } from './password-input'

const schema = z
  .object({
    username: z
      .string()
      .trim()
      .min(1, t('users.createDialog.usernameRequired'))
      .max(128)
      .regex(/^[A-Za-z0-9][A-Za-z0-9._@-]*$/, t('users.createDialog.usernamePattern')),
    display_name: z.string().max(256),
    email: z.union([z.literal(''), z.email(t('users.createDialog.emailInvalid')).max(320)]),
    password: z
      .string()
      .min(12, t('users.createDialog.passwordShort'))
      .max(1024)
      .refine((v) => !v.startsWith('ryx_'), t('users.createDialog.passwordPrefix')),
    confirm: z.string(),
    role_ids: z.array(z.string()),
  })
  .refine((v) => v.password === v.confirm, {
    path: ['confirm'],
    message: t('users.createDialog.confirmMismatch'),
  })
type Values = z.infer<typeof schema>

/**
 * Create a local user. The two password fields never leave this dialog: the form is reset and the
 * dialog unmounted after the call, whatever its outcome, and a username that exists in another
 * source (409 `username_conflict`) is explained at the field instead of a bare error.
 */
export function UserCreateDialog({ onClose }: { onClose: () => void }) {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const roles = useQuery(rolePickerQuery)
  const [createdId, setCreatedId] = useState<string | null>(null)
  // Leaves for the new user's page once the guard has let go of the (now clean) form.
  const { finished, finish } = useFinish(() => {
    if (createdId !== null) void navigate({ to: '/admin/users/$id', params: { id: createdId } })
  })
  const form = useZodForm(schema, {
    defaultValues: {
      username: '',
      display_name: '',
      email: '',
      password: '',
      confirm: '',
      role_ids: [],
    },
  })

  const items: PickerItem[] = (roles.data ?? [])
    .filter((r) => r.name !== 'nx-anonymous')
    .map((r) => ({ id: r.id, label: r.name, description: r.description || undefined }))

  async function onSubmit(values: Values): Promise<void> {
    try {
      const created = await createUser({
        username: values.username,
        password: values.password,
        ...(values.display_name === '' ? {} : { display_name: values.display_name }),
        ...(values.email === '' ? {} : { email: values.email }),
        role_ids: values.role_ids,
      })
      void queryClient.invalidateQueries({ queryKey: userKeys.all })
      toast.success(t('users.createDialog.created', { name: created.data.username }))
      setCreatedId(created.data.id)
      finish()
    } catch (error) {
      // A rejected attempt must not leave the password in the form either.
      form.resetField('password')
      form.resetField('confirm')
      if (isApiError(error) && error.code === 'username_conflict') {
        form.setError(
          'username',
          { type: 'server', message: t('users.createDialog.usernameConflict') },
          { shouldFocus: true },
        )
      } else if (isApiError(error) && error.code === 'already_exists') {
        form.setError(
          'username',
          { type: 'server', message: t('users.createDialog.usernameTaken') },
          { shouldFocus: true },
        )
      } else if (!applyApiFieldErrors(form, error)) {
        toast.apiError(error, t('users.createDialog.saveFailed'))
      }
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t('users.createDialog.title')}</DialogTitle>
          <DialogDescription>{t('users.createDialog.description')}</DialogDescription>
        </DialogHeader>
        <Form form={form} onSubmit={onSubmit} aria-label={t('users.createDialog.title')}>
          <TextField<Values>
            name="username"
            label={t('users.createDialog.username')}
            helper={t('users.createDialog.usernameHelper')}
            required
            autoComplete="off"
            autoCapitalize="none"
            spellCheck={false}
          />
          <div className="grid gap-4 sm:grid-cols-2">
            <TextField<Values>
              name="display_name"
              label={t('users.createDialog.displayName')}
              autoComplete="off"
            />
            <TextField<Values>
              name="email"
              type="email"
              label={t('users.createDialog.email')}
              autoComplete="off"
            />
          </div>
          <div className="grid gap-4 sm:grid-cols-2">
            <PasswordInput<Values>
              name="password"
              label={t('users.createDialog.password')}
              helper={t('users.createDialog.passwordHelper')}
              required
            />
            <PasswordInput<Values>
              name="confirm"
              label={t('users.createDialog.confirm')}
              required
            />
          </div>
          <Controller
            control={form.control}
            name="role_ids"
            render={({ field }) => (
              <MultiPicker
                legend={t('users.createDialog.roles')}
                what={t('users.createDialog.rolesWhat')}
                items={items}
                value={field.value}
                onChange={field.onChange}
                helper={t('users.createDialog.rolesHelper')}
              />
            )}
          />
          <DialogFooter>
            <Button variant="outline" onClick={onClose}>
              {t('common.cancel')}
            </Button>
            <Button type="submit" loading={form.formState.isSubmitting}>
              {t('users.createDialog.submit')}
            </Button>
          </DialogFooter>
        </Form>
        <UnsavedChangesGuard when={form.formState.isDirty && !finished} />
      </DialogContent>
    </Dialog>
  )
}
