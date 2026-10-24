import { useFormContext } from 'react-hook-form'
import { SelectField, TextareaField, TextField } from '@/components/form/form'
import { t } from '@/i18n'
import type { UseFormSetError } from 'react-hook-form'
import { isApiError } from '@/api/errors'
import { pointerToName } from '@/components/form/form'
import { CRITERIA, describeFormCriteria, type CriterionId, type PolicyFormValues } from './criteria'
import type { FormatInfo } from './api'

/** A format's name for people: the server's label, else the lowercase format id. */
export function formatLabel(format: string, formats: readonly FormatInfo[] | undefined): string {
  return formats?.find((f) => f.format === format)?.label ?? format
}

/** Server field pointers (`/criteria/name_pattern`) onto the form's flat field names. */
export function applyPolicyErrors(
  form: { setError: UseFormSetError<PolicyFormValues> },
  error: unknown,
): boolean {
  if (!isApiError(error)) return false
  if (error.code === 'already_exists') {
    form.setError('name', { type: 'server', message: error.message }, { shouldFocus: true })
    return true
  }
  const fields = error.fieldErrors
  if (fields.length === 0) return false
  fields.forEach((f, index) => {
    const name =
      pointerToName(f.field.replace(/^\/criteria(\/|$)/, '$1').replace(/^\//, '')) || 'criteria'
    form.setError(
      name as keyof PolicyFormValues,
      { type: 'server', message: f.message },
      { shouldFocus: index === 0 },
    )
  })
  return true
}

export function CriteriaSummary() {
  const { watch } = useFormContext<PolicyFormValues>()
  const parts = describeFormCriteria(watch())
  return (
    <div className="rounded-md border border-border bg-muted px-3 py-2 text-sm">
      {parts.length === 0 ? (
        <p className="text-muted-foreground">{t('cleanup.criteria.summaryNone')}</p>
      ) : (
        <>
          <p className="font-medium">{t('cleanup.criteria.summaryLead')}</p>
          <ul className="mt-1 list-disc space-y-0.5 pl-5">
            {parts.map((part) => (
              <li key={part} className="break-all">
                {part}
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  )
}

const NUMBER_FIELDS: readonly CriterionId[] = [
  'older_than_days',
  'last_downloaded_days',
  'keep_last_versions',
]

export interface CriteriaFieldsProps {
  /** Criteria the chosen format supports; `null` while unknown (the server validates on save). */
  supported: readonly string[] | null
  formatName: string
}

/**
 * One input per criterion. A criterion the format does not support stays visible but disabled,
 * with the reason next to it (inventory: "disabled with reason"), so the user learns what the
 * format cannot do instead of wondering where the field went.
 */
export function CriteriaFields({ supported, formatName }: CriteriaFieldsProps) {
  const { formState } = useFormContext<PolicyFormValues>()
  const criteriaError = (formState.errors as Record<string, { message?: string } | undefined>)
    .criteria?.message
  const isSupported = (id: CriterionId): boolean => supported === null || supported.includes(id)
  const reason = t('cleanup.criteria.unsupported', { format: formatName })
  return (
    <fieldset className="space-y-4 rounded-lg border border-border p-4">
      <legend className="px-1 text-sm font-semibold">{t('cleanup.criteria.legend')}</legend>
      <p className="text-sm text-muted-foreground">{t('cleanup.criteria.help')}</p>
      <div className="grid gap-4 sm:grid-cols-2">
        {CRITERIA.map((id) => {
          const helper = isSupported(id) ? t(`cleanup.criteria.${id}.helper`) : reason
          const label = t(`cleanup.criteria.${id}.label`)
          if (id === 'release_type') {
            return (
              <SelectField<PolicyFormValues>
                key={id}
                name="release_type"
                label={label}
                helper={helper}
                disabled={!isSupported(id)}
              >
                <option value="">{t('cleanup.criteria.release_type.any')}</option>
                <option value="release">{t('cleanup.criteria.release_type.optionRelease')}</option>
                <option value="prerelease">
                  {t('cleanup.criteria.release_type.optionPrerelease')}
                </option>
              </SelectField>
            )
          }
          return (
            <TextField<PolicyFormValues>
              key={id}
              name={id}
              label={label}
              helper={helper}
              disabled={!isSupported(id)}
              inputMode={NUMBER_FIELDS.includes(id) ? 'numeric' : undefined}
              autoComplete="off"
              spellCheck={false}
              className={id === 'name_pattern' ? 'font-mono' : undefined}
            />
          )
        })}
      </div>
      {criteriaError ? (
        <p role="alert" className="text-sm font-medium text-destructive-text">
          {criteriaError}
        </p>
      ) : null}
      <CriteriaSummary />
    </fieldset>
  )
}

export function BasicFields({
  formats,
  formatLockedBy,
}: {
  formats: readonly FormatInfo[] | undefined
  formatLockedBy: number
}) {
  const { watch } = useFormContext<PolicyFormValues>()
  const current = watch('format')
  // While the format list loads, the select shows the format the form holds, never another one.
  const options = formats?.map((f) => ({ value: f.format, label: f.label })) ?? [
    { value: current, label: current },
  ]
  return (
    <>
      <div className="sm:max-w-md">
        <TextField<PolicyFormValues>
          name="name"
          label={t('cleanup.fields.name')}
          helper={t('cleanup.fields.nameHelper')}
          required
          autoComplete="off"
          className="font-mono"
        />
      </div>
      <div className="sm:max-w-xl">
        <TextareaField<PolicyFormValues>
          name="description"
          label={t('cleanup.fields.description')}
          rows={2}
        />
      </div>
      <div className="sm:max-w-xs">
        <SelectField<PolicyFormValues>
          name="format"
          label={t('cleanup.fields.format')}
          helper={
            formatLockedBy > 0
              ? t('cleanup.fields.formatLocked', { count: formatLockedBy })
              : t('cleanup.fields.formatHelper')
          }
          required
          disabled={formatLockedBy > 0}
        >
          {options.map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </SelectField>
      </div>
    </>
  )
}
