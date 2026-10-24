import '@/lib/zod-setup'
import { z } from 'zod'
import { applyApiFieldErrors, Form, useZodForm } from '@/components/form/form'
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
import { toast } from '@/lib/toast-store'
import { useResetPassword } from './api'
import { PasswordInput } from './password-input'

const schema = z
  .object({
    password: z
      .string()
      .min(12, t('users.createDialog.passwordShort'))
      .max(1024)
      .refine((v) => !v.startsWith('ryx_'), t('users.createDialog.passwordPrefix')),
    confirm: z.string(),
  })
  .refine((v) => v.password === v.confirm, {
    path: ['confirm'],
    message: t('users.createDialog.confirmMismatch'),
  })
type Values = z.infer<typeof schema>

export interface ResetPasswordDialogProps {
  userId: string
  username: string
  onClose: () => void
}

/**
 * An administrator sets a new password for a local account. The password is never shown after
 * the call: the form is reset on success AND on failure, and the dialog is unmounted on success.
 */
export function ResetPasswordDialog({ userId, username, onClose }: ResetPasswordDialogProps) {
  const reset = useResetPassword(userId)
  const form = useZodForm(schema, { defaultValues: { password: '', confirm: '' } })

  async function onSubmit(values: Values): Promise<void> {
    try {
      await reset.mutateAsync(values.password)
      form.reset()
      toast.success(t('users.reset.done', { name: username }))
      onClose()
    } catch (error) {
      form.resetField('password')
      form.resetField('confirm')
      if (!applyApiFieldErrors(form, error)) {
        toast.apiError(error, t('users.reset.failed'))
      }
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('users.reset.title', { name: username })}</DialogTitle>
          <DialogDescription>{t('users.reset.description')}</DialogDescription>
        </DialogHeader>
        <Form
          form={form}
          onSubmit={onSubmit}
          aria-label={t('users.reset.title', { name: username })}
        >
          <PasswordInput<Values>
            name="password"
            label={t('users.reset.password')}
            helper={t('users.createDialog.passwordHelper')}
            required
          />
          <PasswordInput<Values> name="confirm" label={t('users.reset.confirm')} required />
          <DialogFooter>
            <Button variant="outline" onClick={onClose}>
              {t('common.cancel')}
            </Button>
            <Button type="submit" loading={form.formState.isSubmitting}>
              {t('users.reset.submit')}
            </Button>
          </DialogFooter>
        </Form>
      </DialogContent>
    </Dialog>
  )
}
