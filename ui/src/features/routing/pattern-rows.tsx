import { useQuery } from '@tanstack/react-query'
import { CircleAlert, CircleCheck, Plus, Trash2 } from 'lucide-react'
import { useEffect, useRef } from 'react'
import { useFieldArray, useFormContext, useWatch } from 'react-hook-form'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { useDebouncedValue } from '@/hooks/use-debounced-value'
import { t } from '@/i18n'
import { validatePatterns } from './api'
import type { RuleFormValues } from './rule-model'

const LINT_DEBOUNCE_MS = 400

/** Lint verdict per pattern index, from the server's linter (the engine knows what it supports). */
export function usePatternLint(patterns: readonly string[]) {
  const debounced = useDebouncedValue(
    patterns.map((p) => p.trim()).join('\u0000'),
    LINT_DEBOUNCE_MS,
  )
  const list = debounced === '' ? [] : debounced.split('\u0000')
  const query = useQuery({
    queryKey: ['routing-rules', 'lint', debounced],
    queryFn: ({ signal }) => validatePatterns(list, signal),
    enabled: list.some((p) => p !== ''),
    staleTime: 60_000,
    // A linter that cannot be reached must not block editing: the server checks again on save.
    retry: false,
  })
  const byIndex = new Map<number, { offset: number | null; message: string }>()
  for (const issue of query.data?.errors ?? []) {
    byIndex.set(issue.index, { offset: issue.offset, message: issue.message })
  }
  return { byIndex, checking: query.isFetching, ok: query.data?.valid === true }
}

/**
 * The list of patterns, one input per row with its own lint message, added and removed with
 * buttons. The new row takes focus so a keyboard user can keep typing.
 */
export function PatternRows() {
  const { control, register, formState, getValues } = useFormContext<RuleFormValues>()
  const { fields, append, remove } = useFieldArray({ control, name: 'patterns' })
  const watched = useWatch({ control, name: 'patterns' })
  const lint = usePatternLint(watched.map((p) => p.value))
  const focusLast = useRef(false)
  const list = useRef<HTMLUListElement>(null)

  useEffect(() => {
    if (!focusLast.current) return
    focusLast.current = false
    const inputs = list.current?.querySelectorAll<HTMLInputElement>('input')
    inputs?.[inputs.length - 1]?.focus()
  }, [fields.length])

  const rootError = (formState.errors.patterns as { message?: string } | undefined)?.message

  return (
    <fieldset className="space-y-3 rounded-lg border border-border p-4">
      <legend className="px-1 text-sm font-semibold">{t('routing.patterns.legend')}</legend>
      <p className="text-sm text-muted-foreground">{t('routing.patterns.help')}</p>
      <ul ref={list} className="space-y-3">
        {fields.map((field, index) => {
          const formError = formState.errors.patterns?.[index]?.value?.message
          const issue = lint.byIndex.get(index)
          const lintMessage = issue
            ? issue.offset === null
              ? t('routing.patterns.errorPlain', { message: issue.message })
              : t('routing.patterns.errorAt', { offset: issue.offset, message: issue.message })
            : undefined
          const error = formError ?? lintMessage
          const id = `pattern-${field.id}`
          return (
            <li key={field.id} className="space-y-1">
              <Label htmlFor={id}>{t('routing.patterns.row', { index: index + 1 })}</Label>
              <div className="flex items-start gap-2">
                <Input
                  id={id}
                  className="font-mono"
                  placeholder={t('routing.patterns.placeholder')}
                  spellCheck={false}
                  autoComplete="off"
                  aria-invalid={error ? true : undefined}
                  aria-describedby={error ? `${id}-error` : undefined}
                  {...register(`patterns.${index}.value` as const)}
                />
                <Button
                  variant="outline"
                  size="icon"
                  aria-label={t('routing.patterns.remove', { index: index + 1 })}
                  disabled={fields.length === 1 && getValues('patterns.0.value') === ''}
                  onClick={() => remove(index)}
                >
                  <Trash2 aria-hidden="true" />
                </Button>
              </div>
              {error ? (
                <p
                  id={`${id}-error`}
                  className="flex items-start gap-1 text-xs font-medium text-destructive-text"
                >
                  <CircleAlert className="mt-px size-3.5 shrink-0" aria-hidden="true" />
                  <span>{error}</span>
                </p>
              ) : null}
            </li>
          )
        })}
      </ul>
      <div className="flex flex-wrap items-center gap-3">
        <Button
          variant="outline"
          onClick={() => {
            focusLast.current = true
            append({ value: '' })
          }}
        >
          <Plus aria-hidden="true" />
          {t('routing.patterns.add')}
        </Button>
        <p role="status" className="flex items-center gap-1 text-xs text-muted-foreground">
          {lint.checking ? (
            t('routing.patterns.checking')
          ) : lint.ok && lint.byIndex.size === 0 ? (
            <>
              <CircleCheck className="size-3.5 text-success-text" aria-hidden="true" />
              {t('routing.patterns.valid')}
            </>
          ) : null}
        </p>
      </div>
      {rootError ? (
        <p role="alert" className="text-sm font-medium text-destructive-text">
          {rootError}
        </p>
      ) : null}
    </fieldset>
  )
}
