import { useEffect } from 'react'
import { z } from 'zod'
import { isApiError } from '@/api/errors'
import { applyApiFieldErrors, Form, TextField, useZodForm } from '@/components/form/form'
import { ErrorPanel } from '@/components/error-panel'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Label } from '@/components/ui/label'
import { t } from '@/i18n'
import { useCreateClientToken } from './api'

export type TokenAccess = 'read' | 'write'

export interface CreatedClientToken {
  secret: string
}

export interface CreateClientTokenDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  repository: string
  /** Offer "read and publish" only when the caller may write to the repository. */
  canWrite: boolean
  defaultTtlDays: number
  maxTtlDays: number
  /** Receives the one time secret. The caller keeps it in memory for this page view only. */
  onCreated: (token: CreatedClientToken) => void
}

const schema = (maxDays: number) =>
  z.object({
    name: z.string().trim().min(1, t('clientSetup.tokenDialog.nameRequired')).max(128),
    days: z
      .string()
      .regex(/^\d+$/, t('clientSetup.tokenDialog.daysInvalid', { max: maxDays }))
      .refine((v) => Number(v) >= 1 && Number(v) <= maxDays, {
        message: t('clientSetup.tokenDialog.daysInvalid', { max: maxDays }),
      }),
    access: z.enum(['read', 'write']),
  })

type Values = z.infer<ReturnType<typeof schema>>

/** Scopes only narrow the owner's rights (docs/api-conventions.md, 11.6): browse and read, plus write on request. */
export function scopesFor(repository: string, access: TokenAccess): string[] {
  const read = [`repo:${repository}:browse`, `repo:${repository}:read`]
  return access === 'write' ? [...read, `repo:${repository}:write`] : read
}

/**
 * The personal token dialog of the client setup panel. The secret goes to `onCreated` and is
 * never kept here: the mutation is not cached either (see `useCreateClientToken`).
 */
export function CreateClientTokenDialog(props: CreateClientTokenDialogProps) {
  return (
    <Dialog open={props.open} onOpenChange={props.onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('clientSetup.tokenDialog.title')}</DialogTitle>
          <DialogDescription>
            {t('clientSetup.tokenDialog.description', { name: props.repository })}
          </DialogDescription>
        </DialogHeader>
        <TokenForm {...props} />
      </DialogContent>
    </Dialog>
  )
}

function TokenForm({
  onOpenChange,
  repository,
  canWrite,
  defaultTtlDays,
  maxTtlDays,
  onCreated,
}: CreateClientTokenDialogProps) {
  const mutation = useCreateClientToken()
  const form = useZodForm<Values>(schema(maxTtlDays), {
    defaultValues: {
      name: t('clientSetup.tokenNameDefault', { repository }),
      days: String(Math.min(defaultTtlDays, maxTtlDays)),
      access: 'read',
    },
  })
  const { setFocus } = form
  useEffect(() => setFocus('name'), [setFocus])

  async function submit(values: Values): Promise<void> {
    try {
      const created = await mutation.mutateAsync({
        name: values.name,
        expires_in_days: Number(values.days),
        scopes: scopesFor(repository, values.access),
      })
      onCreated({ secret: created.secret })
      onOpenChange(false)
    } catch (error) {
      applyApiFieldErrors(form, error)
    }
  }

  const access = form.watch('access')
  return (
    <Form form={form} onSubmit={submit}>
      <TextField<Values> name="name" label={t('clientSetup.tokenDialog.name')} required />
      <TextField<Values>
        name="days"
        label={t('clientSetup.tokenDialog.expires')}
        helper={t('clientSetup.tokenDialog.expiresHelper', { max: maxTtlDays })}
        inputMode="numeric"
        required
      />
      <fieldset className="space-y-2">
        <legend className="mb-1 text-sm leading-none font-medium">
          {t('clientSetup.tokenDialog.access')}
        </legend>
        {(['read', ...(canWrite ? ['write'] : [])] as TokenAccess[]).map((value) => (
          <div key={value} className="flex items-center gap-2">
            <input
              id={`token-access-${value}`}
              type="radio"
              value={value}
              checked={access === value}
              onChange={() => form.setValue('access', value)}
              className="size-4 accent-[var(--color-accent)]"
            />
            <Label htmlFor={`token-access-${value}`}>
              {t(`clientSetup.tokenDialog.access_${value}`)}
            </Label>
          </div>
        ))}
      </fieldset>
      {mutation.error && !applyHasFields(mutation.error) ? (
        <ErrorPanel error={mutation.error} title={t('clientSetup.tokenDialog.failed')} />
      ) : null}
      <DialogFooter>
        <Button variant="outline" onClick={() => onOpenChange(false)} disabled={mutation.isPending}>
          {t('common.cancel')}
        </Button>
        <Button type="submit" loading={mutation.isPending}>
          {t('clientSetup.tokenDialog.create')}
        </Button>
      </DialogFooter>
    </Form>
  )
}

function applyHasFields(error: unknown): boolean {
  return isApiError(error) && error.fieldErrors.length > 0
}
