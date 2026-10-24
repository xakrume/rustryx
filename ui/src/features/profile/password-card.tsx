import '@/lib/zod-setup'
import { z } from 'zod'
import { isApiError } from '@/api/errors'
import { applyApiFieldErrors, Form, useZodForm } from '@/components/form/form'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { PasswordInput } from '@/features/users/password-input'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { changeMyPassword } from './api'

const schema = z
  .object({
    current_password: z.string().min(1, t('profile.password.currentRequired')),
    new_password: z
      .string()
      .min(12, t('profile.password.tooShort'))
      .max(1024)
      .refine((v) => !v.startsWith('ryx_'), t('profile.password.prefix')),
    confirm: z.string(),
  })
  .refine((v) => v.new_password === v.confirm, {
    path: ['confirm'],
    message: t('profile.password.mismatch'),
  })
type Values = z.infer<typeof schema>

/**
 * Change the own local password. The three fields live only in the form: after the call, the
 * form is reset whatever the result, so no password stays on the page. External accounts get an
 * explanation instead of a form (their password belongs to the identity provider).
 */
export function PasswordCard({ source }: { source: 'local' | 'oidc' | 'ldap' }) {
  const form = useZodForm(schema, {
    defaultValues: { current_password: '', new_password: '', confirm: '' },
  })

  async function onSubmit(values: Values): Promise<void> {
    try {
      await changeMyPassword(values.current_password, values.new_password)
      form.reset()
      toast.success(t('profile.password.changed'))
    } catch (error) {
      // resetField, not reset: reset drops the registered inputs until the next render, so the
      // focus call below would find nothing to focus.
      form.resetField('current_password')
      form.resetField('new_password')
      form.resetField('confirm')
      if (isApiError(error) && error.status === 401 && error.code === 'invalid_credentials') {
        form.setError('current_password', {
          type: 'server',
          message: t('profile.password.wrongCurrent'),
        })
        form.setFocus('current_password')
      } else if (!applyApiFieldErrors(form, error)) {
        toast.apiError(error, t('profile.password.failed'))
      }
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('profile.password.title')}</CardTitle>
        {source === 'local' ? (
          <CardDescription>{t('profile.password.description')}</CardDescription>
        ) : null}
      </CardHeader>
      <CardContent>
        {source !== 'local' ? (
          <Alert title={t('profile.password.externalTitle')}>
            {t('profile.password.externalLocal', { source: t(`identity.source.${source}`) })}
          </Alert>
        ) : (
          <Form form={form} onSubmit={onSubmit} aria-label={t('profile.password.title')}>
            <PasswordInput<Values>
              name="current_password"
              label={t('profile.password.current')}
              autoComplete="current-password"
              required
            />
            <PasswordInput<Values>
              name="new_password"
              label={t('profile.password.new')}
              helper={t('profile.password.rules')}
              required
            />
            <PasswordInput<Values> name="confirm" label={t('profile.password.confirm')} required />
            <Button type="submit" loading={form.formState.isSubmitting}>
              {t('profile.password.submit')}
            </Button>
          </Form>
        )}
      </CardContent>
    </Card>
  )
}
