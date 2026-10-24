import '@/lib/zod-setup'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useWatch } from 'react-hook-form'
import { z } from 'zod'
import { isApiError } from '@/api/errors'
import {
  applyApiFieldErrors,
  Form,
  SelectField,
  TextField,
  useZodForm,
} from '@/components/form/form'
import { UnsavedChangesGuard } from '@/components/form/unsaved-changes-guard'
import { Alert } from '@/components/ui/alert'
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
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { createMapping, mappingKeys, updateMapping, type MappingView, type Provider } from './api'

const schema = z.object({
  // Not trimmed on purpose: the group is matched byte for byte, so what is typed is what is sent.
  external_group: z
    .string()
    .min(1, t('mappings.dialog.groupRequired'))
    .max(512, t('mappings.dialog.groupTooLong')),
  role_id: z.string().min(1, t('mappings.dialog.roleRequired')),
  priority: z.string().regex(/^-?\d{1,9}$/, t('mappings.dialog.priorityInvalid')),
})
type Values = z.infer<typeof schema>

const HIDDEN_ROLES = new Set(['nx-admin', 'nx-anonymous'])

export interface MappingDialogProps {
  provider: Provider
  /** The mapping to edit, or null to add one. */
  mapping: MappingView | null
  onClose: () => void
}

/** Warnings that never block: they say why a value will probably match nothing. */
function groupWarnings(provider: Provider, group: string): string[] {
  const warnings: string[] = []
  if (group !== '' && group !== group.trim()) warnings.push(t('mappings.dialog.warnSpaces'))
  if (provider === 'oidc' && group !== '' && !group.startsWith('/')) {
    warnings.push(t('mappings.dialog.warnSlash'))
  }
  if (provider === 'ldap' && group !== '' && !group.includes('=')) {
    warnings.push(t('mappings.dialog.warnDn'))
  }
  return warnings
}

/**
 * Add or change a group mapping. The group is entered and sent exactly as typed (never trimmed or
 * case folded) under a permanent notice that matching is exact and case sensitive. Provider and
 * group are fixed after creation, so editing changes only the role and the priority. nx-admin and
 * nx-anonymous are not offered: administrators come only from the configured admin groups.
 */
export function MappingDialog({ provider, mapping, onClose }: MappingDialogProps) {
  const queryClient = useQueryClient()
  const roles = useQuery(rolePickerQuery)
  const form = useZodForm(schema, {
    defaultValues: {
      external_group: mapping?.external_group ?? '',
      role_id: mapping?.role_id ?? '',
      priority: String(mapping?.priority ?? 100),
    },
  })
  const { finished, finish } = useFinish(onClose)
  const group = useWatch({ control: form.control, name: 'external_group' })
  const warnings = mapping === null ? groupWarnings(provider, group) : []
  const title =
    mapping === null
      ? t('mappings.dialog.createTitle', { provider: t(`mappings.tabs.${provider}`) })
      : t('mappings.dialog.editTitle', { group: mapping.external_group })

  async function onSubmit(values: Values): Promise<void> {
    try {
      const priority = Number(values.priority)
      if (mapping === null) {
        const created = await createMapping({
          provider,
          external_group: values.external_group,
          role_id: values.role_id,
          priority,
        })
        toast.success(t('mappings.dialog.created', { group: created.data.external_group }))
      } else {
        const saved = await updateMapping(mapping.id, { role_id: values.role_id, priority })
        toast.success(t('mappings.dialog.saved', { group: saved.external_group }))
      }
      void queryClient.invalidateQueries({ queryKey: mappingKeys.all })
      void queryClient.invalidateQueries({ queryKey: ['roles'] })
      finish()
    } catch (error) {
      if (isApiError(error) && error.code === 'already_exists') {
        form.setError(
          'external_group',
          { type: 'server', message: t('mappings.dialog.duplicate') },
          { shouldFocus: true },
        )
      } else if (!applyApiFieldErrors(form, error)) {
        toast.apiError(error, t('mappings.dialog.saveFailed'))
      }
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{t('mappings.dialog.description')}</DialogDescription>
        </DialogHeader>
        <Alert variant="warning" title={t('mappings.exact.title')} className="mb-3">
          {t('mappings.exact.body')}
        </Alert>
        <Form form={form} onSubmit={onSubmit} aria-label={title}>
          <TextField<Values>
            name="external_group"
            label={t('mappings.dialog.group')}
            helper={
              mapping !== null
                ? t('mappings.dialog.groupFixed')
                : provider === 'oidc'
                  ? t('mappings.dialog.groupHelperOidc')
                  : t('mappings.dialog.groupHelperLdap')
            }
            required
            readOnly={mapping !== null}
            autoComplete="off"
            autoCapitalize="none"
            autoCorrect="off"
            spellCheck={false}
            className="font-mono"
          />
          {warnings.length > 0 || mapping === null ? (
            <ul className="space-y-1 text-xs text-warning-text" aria-label={t('form.fixErrors')}>
              {warnings.map((warning) => (
                <li key={warning}>{warning}</li>
              ))}
              {group !== '' ? <li>{t('mappings.dialog.warnUnknown')}</li> : null}
            </ul>
          ) : null}
          <SelectField<Values>
            name="role_id"
            label={t('mappings.dialog.role')}
            helper={t('mappings.dialog.roleHelper')}
            required
          >
            <option value="">{t('mappings.dialog.roleNone')}</option>
            {(roles.data ?? [])
              .filter((r) => !HIDDEN_ROLES.has(r.name))
              .map((r) => (
                <option key={r.id} value={r.id}>
                  {r.name}
                </option>
              ))}
          </SelectField>
          <TextField<Values>
            name="priority"
            label={t('mappings.dialog.priorityLabel')}
            helper={t('mappings.dialog.priorityHelper')}
            inputMode="numeric"
            autoComplete="off"
            className="max-w-32 tabular-nums"
          />
          <DialogFooter>
            <Button variant="outline" onClick={onClose}>
              {t('common.cancel')}
            </Button>
            <Button type="submit" loading={form.formState.isSubmitting}>
              {mapping === null ? t('mappings.dialog.create') : t('mappings.dialog.save')}
            </Button>
          </DialogFooter>
        </Form>
        <UnsavedChangesGuard when={form.formState.isDirty && !finished} />
      </DialogContent>
    </Dialog>
  )
}
