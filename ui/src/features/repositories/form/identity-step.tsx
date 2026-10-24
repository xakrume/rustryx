import { useQuery } from '@tanstack/react-query'
import { CircleCheck } from 'lucide-react'
import { useId } from 'react'
import { useFormContext } from 'react-hook-form'
import { FormField } from '@/components/form/form-field'
import { Input } from '@/components/ui/input'
import { useDebouncedValue } from '@/hooks/use-debounced-value'
import { t } from '@/i18n'
import { cn } from '@/lib/utils'
import { formatsQuery, repositoryLookupQuery, type FormatInfo, type RepoType } from '../api'
import { formatLabel } from '../repo-meta'
import { configFields, configValuesFrom } from './format-config'
import type { RepositoryFormValues } from './values'

interface RadioCardOption {
  value: string
  label: string
  description?: string
  disabled?: boolean
}

interface RadioCardsProps {
  legend: string
  name: string
  value: string
  options: readonly RadioCardOption[]
  onChange: (value: string) => void
  error?: string
  className?: string
}

/**
 * A radio group drawn as cards. Real radio inputs, so arrow keys, grouping and the screen reader
 * announcement come from the browser; the checked card is marked by the radio dot and a heavier
 * border, never by color alone.
 */
export function RadioCards({
  legend,
  name,
  value,
  options,
  onChange,
  error,
  className,
}: RadioCardsProps) {
  const errorId = useId()
  return (
    <fieldset className="space-y-2" aria-describedby={error ? errorId : undefined}>
      <legend className="mb-1.5 text-sm leading-none font-medium">
        {legend}
        <span aria-hidden="true" className="ml-0.5 text-destructive-text">
          *
        </span>
        <span className="sr-only"> ({t('form.required')})</span>
      </legend>
      <div className={cn('grid gap-2', className)}>
        {options.map((option) => (
          <label
            key={option.value}
            className={cn(
              'flex cursor-pointer items-start gap-2 rounded-md border border-input bg-card p-3 text-sm has-[:checked]:border-2 has-[:checked]:border-accent has-[:focus-visible]:ring-2 has-[:focus-visible]:ring-ring',
              option.disabled && 'cursor-not-allowed opacity-60',
            )}
          >
            <input
              type="radio"
              name={name}
              value={option.value}
              checked={value === option.value}
              disabled={option.disabled}
              onChange={() => onChange(option.value)}
              className="mt-0.5 size-4 shrink-0 accent-accent"
            />
            <span className="min-w-0">
              <span className="block font-medium">{option.label}</span>
              {option.description ? (
                <span className="block text-xs text-muted-foreground">{option.description}</span>
              ) : null}
            </span>
          </label>
        ))}
      </div>
      {error ? (
        <p id={errorId} role="alert" className="text-xs font-medium text-destructive-text">
          {error}
        </p>
      ) : null}
    </fieldset>
  )
}

/**
 * The live check of the name field (docs/ui-page-inventory.md, 7.2: "name taken, live check").
 * It waits until the typing paused and the name is well formed, then asks the list endpoint.
 * A repository hidden from the caller is not found here; the 409 `already_exists` of the
 * create call is the final answer and lands on this same field.
 */
export function useNameCheck(name: string): { taken: boolean; available: boolean } {
  const debounced = useDebouncedValue(name.trim(), 400)
  const syntaxOk = /^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(debounced) && !debounced.includes('..')
  const lookup = useQuery({ ...repositoryLookupQuery(debounced), enabled: syntaxOk })
  const settled = syntaxOk && debounced === name.trim()
  return { taken: settled && lookup.data === true, available: settled && lookup.data === false }
}

const TYPES: readonly RepoType[] = ['hosted', 'proxy', 'group']

function supports(info: FormatInfo | undefined, type: RepoType): boolean {
  return info === undefined ? true : info.supports[type]
}

/** Step 1 of the wizard: the name (checked live), the format and the type. */
export function IdentityStep() {
  const { register, watch, setValue, formState } = useFormContext<RepositoryFormValues>()
  const formats = useQuery(formatsQuery)
  const name = watch('name')
  const format = watch('format')
  const repoType = watch('repo_type')
  const { taken, available } = useNameCheck(name)
  const nameError =
    formState.errors.name?.message ?? (taken ? t('repoForm.errors.nameTaken') : undefined)

  function pickFormat(value: string): void {
    const info = formats.data?.items.find((f) => f.format === value)
    setValue('format', value, { shouldDirty: true, shouldTouch: true, shouldValidate: true })
    // Everything below depends on the format: its options, its default write policy, the policies
    // and the members that fit it. Start those from the format's own defaults.
    setValue('config', configValuesFrom(configFields(info?.config_schema), {}), {
      shouldDirty: true,
    })
    setValue('cleanup_policy_ids', [], { shouldDirty: true })
    setValue('members', [], { shouldDirty: true })
    if (info) {
      setValue('write_policy', info.default_write_policy, { shouldDirty: true })
      if (!info.supports[repoType]) {
        const first = TYPES.find((type) => info.supports[type])
        if (first) setValue('repo_type', first, { shouldDirty: true })
      }
    }
  }

  const current = formats.data?.items.find((f) => f.format === format)
  return (
    <div className="space-y-5">
      <FormField
        label={t('repoForm.identity.name')}
        required
        error={nameError}
        helper={
          available ? (
            <span className="inline-flex items-center gap-1 text-success-text">
              <CircleCheck className="size-3.5" aria-hidden="true" />
              {t('repoForm.identity.nameAvailable')}
            </span>
          ) : (
            t('repoForm.identity.nameHelper')
          )
        }
      >
        {(control) => (
          <Input
            {...control}
            {...register('name')}
            autoComplete="off"
            spellCheck={false}
            className="block max-w-md font-mono"
            data-autofocus=""
          />
        )}
      </FormField>

      <RadioCards
        legend={t('repoForm.identity.format')}
        name="format"
        value={format}
        onChange={pickFormat}
        error={formState.errors.format?.message}
        className="sm:grid-cols-2 lg:grid-cols-4"
        options={(formats.data?.items ?? []).map((f) => ({
          value: f.format,
          label: f.label || formatLabel(f.format),
          description: [
            f.supports.hosted && t('repos.type.hosted'),
            f.supports.proxy && t('repos.type.proxy'),
            f.supports.group && t('repos.type.group'),
          ]
            .filter(Boolean)
            .join(', '),
        }))}
      />

      <RadioCards
        legend={t('repoForm.identity.type')}
        name="repo_type"
        value={repoType}
        onChange={(value) =>
          setValue('repo_type', value as RepoType, { shouldDirty: true, shouldTouch: true })
        }
        className="sm:grid-cols-3"
        options={TYPES.map((type) => ({
          value: type,
          label: t(`repos.type.${type}`),
          description: supports(current, type)
            ? t(`repoForm.identity.typeHelp.${type}`)
            : t('repoForm.identity.typeUnsupported', { format: formatLabel(format) }),
          disabled: !supports(current, type),
        }))}
      />
    </div>
  )
}
