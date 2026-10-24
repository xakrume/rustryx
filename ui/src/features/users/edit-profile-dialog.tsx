import '@/lib/zod-setup'
import { z } from 'zod'
import { Form, TextField, useZodForm } from '@/components/form/form'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { t } from '@/i18n'
import type { UserPatch, UserView } from './api'

const schema = z.object({
  display_name: z.string().max(256),
  email: z.union([z.literal(''), z.email(t('users.createDialog.emailInvalid')).max(320)]),
})
type Values = z.infer<typeof schema>

export interface EditProfileDialogProps {
  user: UserView
  pending: boolean
  /** Sends the merge patch; the caller closes the dialog when it succeeded. */
  onSave: (patch: UserPatch) => void
  onClose: () => void
}

/** Display name and e-mail of a LOCAL account. External accounts never reach this dialog. */
export function EditProfileDialog({ user, pending, onSave, onClose }: EditProfileDialogProps) {
  const form = useZodForm(schema, {
    defaultValues: { display_name: user.display_name ?? '', email: user.email ?? '' },
  })
  // Only changed fields go into the merge patch: an untouched field must not be rewritten.
  function submit(values: Values): void {
    const patch: UserPatch = {
      ...(values.display_name !== (user.display_name ?? '')
        ? { display_name: values.display_name }
        : {}),
      ...(values.email !== (user.email ?? '') && values.email !== ''
        ? { email: values.email }
        : {}),
    }
    if (Object.keys(patch).length === 0) onClose()
    else onSave(patch)
  }
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('users.detail.profileTitle', { name: user.username })}</DialogTitle>
          <DialogDescription>{t('users.detail.groupsSeenHelper')}</DialogDescription>
        </DialogHeader>
        <Form form={form} onSubmit={submit} aria-label={t('users.detail.editProfile')}>
          <TextField<Values>
            name="display_name"
            label={t('users.detail.displayNameLabel')}
            autoComplete="off"
          />
          <TextField<Values>
            name="email"
            type="email"
            label={t('users.detail.emailLabel')}
            autoComplete="off"
          />
          <DialogFooter>
            <Button variant="outline" onClick={onClose}>
              {t('common.cancel')}
            </Button>
            <Button type="submit" loading={pending}>
              {t('users.detail.profileSave')}
            </Button>
          </DialogFooter>
        </Form>
      </DialogContent>
    </Dialog>
  )
}
