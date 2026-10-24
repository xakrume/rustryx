import '@/lib/zod-setup'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { Controller } from 'react-hook-form'
import { z } from 'zod'
import { applyApiFieldErrors, Form, TextField, useZodForm } from '@/components/form/form'
import { FormField } from '@/components/form/form-field'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Select } from '@/components/ui/select'
import { useMe } from '@/features/auth/use-permissions'
import { repositoryChoicesQuery } from '@/features/privileges/api'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { createMyToken, tokenKeys, type ApiTokenCreated } from './api'
import { ScopePicker } from './scope-picker'
import { scopesOf, type ScopePreset, type ScopeRow } from './scopes'

const schema = z.object({
  name: z
    .string()
    .trim()
    .min(1, t('tokens.create.nameRequired'))
    .max(128, t('tokens.create.nameTooLong')),
  expires_in_days: z.string(),
})
type Values = z.infer<typeof schema>

const CHOICES = [7, 30, 60, 90, 180, 365] as const

export interface CreateTokenDialogProps {
  /**
   * Called with the created token (the only copy of its secret) after the dialog is done. The
   * caller shows it once and must forget it; this dialog keeps nothing.
   */
  onCreated: (created: ApiTokenCreated) => void
  onClose: () => void
}

/**
 * Create a personal token: a name, an expiry from the choices the token policy allows (there is
 * no "never"), and what it may do. The secret comes back once and is handed to the caller; it is
 * never put in a query cache or a form value.
 */
export function CreateTokenDialog({ onCreated, onClose }: CreateTokenDialogProps) {
  const me = useMe()
  const queryClient = useQueryClient()
  const repositories = useQuery(repositoryChoicesQuery)
  const policy = me?.token_policy
  const max = policy?.max_ttl_days ?? 365
  const fallback = policy?.default_ttl_days ?? 90
  const options = CHOICES.filter((days) => days <= max)
  // A policy value between the fixed choices is still offered, so the default is always there.
  const choices = [...new Set([...options, fallback])].sort((a, b) => a - b)

  const form = useZodForm(schema, {
    defaultValues: { name: '', expires_in_days: String(fallback) },
  })
  const [preset, setPreset] = useState<ScopePreset>('full')
  const [rows, setRows] = useState<ScopeRow[]>([
    { key: 1, repository: '', actions: ['browse', 'read'] },
  ])
  const [scopeError, setScopeError] = useState<string | undefined>()

  async function onSubmit(values: Values): Promise<void> {
    const scopes = scopesOf(preset, rows)
    if (preset === 'custom' && scopes.length === 0) {
      setScopeError(t('tokens.create.scopesRequired'))
      return
    }
    if (scopes.length > 64) {
      setScopeError(t('tokens.create.scopeLimit'))
      return
    }
    setScopeError(undefined)
    try {
      const created = await createMyToken({
        name: values.name,
        expires_in_days: Number(values.expires_in_days),
        scopes,
      })
      // The list shows the new token (prefix only) and users show their token count.
      void queryClient.invalidateQueries({ queryKey: tokenKeys.all })
      void queryClient.invalidateQueries({ queryKey: ['users'] })
      toast.success(t('tokens.create.createdToast', { name: created.name }))
      form.reset()
      onCreated(created)
      onClose()
    } catch (error) {
      if (!applyApiFieldErrors(form, error)) toast.apiError(error, t('tokens.create.failed'))
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>{t('tokens.create.title')}</DialogTitle>
          <DialogDescription>{t('tokens.create.description')}</DialogDescription>
        </DialogHeader>
        <Form form={form} onSubmit={onSubmit} aria-label={t('tokens.create.title')}>
          <TextField<Values>
            name="name"
            label={t('tokens.create.name')}
            helper={t('tokens.create.nameHelper')}
            required
            autoComplete="off"
            spellCheck={false}
          />
          <Controller
            control={form.control}
            name="expires_in_days"
            render={({ field }) => (
              <FormField
                label={t('tokens.create.expiry')}
                helper={t('tokens.create.expiryHelper', { max })}
              >
                {(control) => (
                  <Select
                    {...control}
                    value={field.value}
                    onChange={field.onChange}
                    onBlur={field.onBlur}
                  >
                    {choices.map((days) => (
                      <option key={days} value={days}>
                        {days === fallback
                          ? t('tokens.create.defaultMarker', {
                              label: t('tokens.create.days', { count: days }),
                            })
                          : t('tokens.create.days', { count: days })}
                      </option>
                    ))}
                  </Select>
                )}
              </FormField>
            )}
          />
          <ScopePicker
            preset={preset}
            onPresetChange={(next) => {
              setPreset(next)
              setScopeError(undefined)
            }}
            rows={rows}
            onRowsChange={setRows}
            repositories={(repositories.data ?? []).map((r) => r.name)}
            error={scopeError}
          />
          <DialogFooter>
            <Button variant="outline" onClick={onClose}>
              {t('common.cancel')}
            </Button>
            <Button type="submit" loading={form.formState.isSubmitting}>
              {t('tokens.create.submit')}
            </Button>
          </DialogFooter>
        </Form>
      </DialogContent>
    </Dialog>
  )
}
