import '@/lib/zod-setup'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Lock } from 'lucide-react'
import { useRef, useState } from 'react'
import { Controller, useWatch } from 'react-hook-form'
import { z } from 'zod'
import { isApiError } from '@/api/errors'
import { ConflictDialog } from '@/components/conflict-dialog'
import { ErrorPanel } from '@/components/error-panel'
import {
  applyApiFieldErrors,
  Form,
  TextareaField,
  TextField,
  useZodForm,
} from '@/components/form/form'
import { UnsavedChangesGuard } from '@/components/form/unsaved-changes-guard'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Skeleton } from '@/components/ui/skeleton'
import { useUpdateResource } from '@/hooks/use-update-resource'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import {
  createSelector,
  selectorKeys,
  selectorQuery,
  updateSelector,
  validateExpression,
  type CompileIssue,
  type SelectorInput,
  type SelectorView,
} from './api'
import { useFinish } from '@/features/roles/use-finish'
import { ExpressionField } from './expression-field'
import { LanguageHelp } from './language-help'
import { SelectorPreview } from './selector-preview'
import { SelectorTester } from './selector-tester'
import { useLiveValidation } from './use-live-validation'

const MAX_EXPRESSION_BYTES = 1024

const schema = z.object({
  name: z
    .string()
    .trim()
    .min(1, t('selectors.editor.nameRequired'))
    .max(128, t('selectors.editor.nameTooLong')),
  description: z.string().max(1024),
  expression: z.string().min(1, t('selectors.editor.expressionRequired')),
})
type Values = z.infer<typeof schema>

const valuesOf = (selector: SelectorView | null): Values => ({
  name: selector?.name ?? '',
  description: selector?.description ?? '',
  expression: selector?.expression ?? '',
})

/** `details.compile_errors` of a 400 `invalid_selector`, with malformed entries dropped. */
function compileErrorsOf(error: unknown): CompileIssue[] {
  if (!isApiError(error)) return []
  const raw = error.details?.compile_errors
  if (!Array.isArray(raw)) return []
  return raw.filter(
    (e): e is CompileIssue =>
      typeof e === 'object' && e !== null && typeof (e as CompileIssue).message === 'string',
  )
}

export interface SelectorEditorProps {
  selectorId: string | null
  onClose: () => void
}

/**
 * The content selector editor. The expression is compiled by the server while the user types
 * and again right before saving, so a selector that does not compile is never saved and its
 * problems are shown with line and column. A test panel evaluates the expression against sample
 * input, and a preview runs it on a repository. Saved with `If-Match`.
 */
export function SelectorEditor({ selectorId, onClose }: SelectorEditorProps) {
  const detail = useQuery({
    ...selectorQuery(selectorId ?? ''),
    enabled: selectorId !== null,
    refetchOnWindowFocus: false,
  })
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-5xl">
        {selectorId !== null && detail.isPending ? (
          <>
            <DialogHeader>
              <DialogTitle>{t('common.loading')}</DialogTitle>
              <DialogDescription className="sr-only">{t('common.loading')}</DialogDescription>
            </DialogHeader>
            <div aria-busy="true" className="space-y-3">
              <Skeleton className="h-9 w-full" />
              <Skeleton className="h-32 w-full" />
            </div>
          </>
        ) : selectorId !== null && detail.isError ? (
          <>
            <DialogHeader>
              <DialogTitle>{t('selectors.editor.loadFailed')}</DialogTitle>
              <DialogDescription className="sr-only">
                {t('selectors.editor.loadFailed')}
              </DialogDescription>
            </DialogHeader>
            <ErrorPanel
              error={detail.error}
              onRetry={() => void detail.refetch()}
              title={t('selectors.editor.loadFailed')}
            />
            <DialogFooter>
              <Button variant="outline" onClick={onClose}>
                {t('selectors.editor.close')}
              </Button>
            </DialogFooter>
          </>
        ) : (
          <SelectorForm selector={detail.data?.data ?? null} onClose={onClose} />
        )}
      </DialogContent>
    </Dialog>
  )
}

function SelectorForm({
  selector,
  onClose,
}: {
  selector: SelectorView | null
  onClose: () => void
}) {
  const queryClient = useQueryClient()
  const form = useZodForm(schema, { defaultValues: valuesOf(selector) })
  const { finished, finish } = useFinish(onClose)
  const expression = useWatch({ control: form.control, name: 'expression' })
  const live = useLiveValidation(expression)
  // Problems from a save attempt or a test run, for THIS text: any edit makes them stale.
  const [attempt, setAttempt] = useState<{ text: string; issues: readonly CompileIssue[] } | null>(
    null,
  )
  const issues = attempt !== null && attempt.text === expression ? attempt.issues : null
  const boxRef = useRef<HTMLTextAreaElement | null>(null)
  const report = (found: readonly CompileIssue[]): void =>
    setAttempt({ text: expression, issues: found })

  const update = useUpdateResource<SelectorView, SelectorInput>({
    queryKey: selectorKeys.detail(selector?.id ?? ''),
    resourceName: selector === null ? '' : `content selector ${selector.name}`,
    update: (input, etag) => updateSelector(selector!.id, input, etag),
    fetchCurrent: () => queryClient.fetchQuery({ ...selectorQuery(selector!.id), staleTime: 0 }),
    invalidate: [selectorKeys.all, ['privileges']],
    copyChanges: (input) => JSON.stringify(input, null, 2),
    onSuccess: (saved) => {
      toast.success(t('selectors.editor.saved', { name: saved.name }))
      finish()
    },
    onError: (error) => failed(error),
  })

  function failed(error: unknown): void {
    const compile = compileErrorsOf(error)
    if (compile.length > 0) {
      report(compile)
      boxRef.current?.focus()
      return
    }
    if (isApiError(error) && error.code === 'already_exists') {
      form.setError('name', { type: 'server', message: error.message }, { shouldFocus: true })
      return
    }
    if (!applyApiFieldErrors(form, error)) toast.apiError(error, t('selectors.editor.saveFailed'))
  }

  async function onSubmit(values: Values): Promise<void> {
    // Never rely on the debounced live result alone: compile the exact text that is saved.
    const verdict =
      live.status === 'valid' && live.checked === values.expression
        ? null
        : await validateExpression(values.expression).catch((error: unknown) => {
            failed(error)
            return undefined
          })
    if (verdict === undefined) return
    if (verdict !== null && !verdict.valid) {
      report(verdict.errors)
      boxRef.current?.focus()
      return
    }
    const input: SelectorInput = { ...values }
    if (selector !== null) {
      update.mutate(input)
      return
    }
    try {
      const created = await createSelector(input)
      void queryClient.invalidateQueries({ queryKey: selectorKeys.all })
      toast.success(t('selectors.editor.created', { name: created.data.name }))
      finish()
    } catch (error) {
      failed(error)
    }
  }

  const title =
    selector === null
      ? t('selectors.editor.createTitle')
      : t('selectors.editor.editTitle', { name: selector.name })

  return (
    <>
      <DialogHeader>
        <DialogTitle>{title}</DialogTitle>
        <DialogDescription>
          {selector === null
            ? t('selectors.editor.createDescription')
            : t('selectors.editor.editDescription')}
        </DialogDescription>
      </DialogHeader>
      <Form form={form} onSubmit={onSubmit} aria-label={title}>
        <div className="grid gap-6 lg:grid-cols-2">
          <div className="space-y-4">
            <TextField<Values>
              name="name"
              label={t('selectors.editor.name')}
              required
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
            />
            <TextareaField<Values>
              name="description"
              label={t('selectors.editor.descriptionLabel')}
              rows={2}
            />
            <Controller
              control={form.control}
              name="expression"
              render={({ field, fieldState }) => (
                <ExpressionField
                  value={field.value}
                  onChange={field.onChange}
                  onBlur={field.onBlur}
                  validation={live}
                  issues={issues}
                  error={fieldState.error?.message}
                  limit={MAX_EXPRESSION_BYTES}
                  textareaRef={boxRef}
                />
              )}
            />
            <LanguageHelp />
            {selector !== null && selector.used_by_privileges.length > 0 ? (
              <div className="space-y-1 text-sm">
                <p className="flex items-center gap-1 font-medium">
                  <Lock className="size-3.5" aria-hidden="true" />
                  {t('selectors.editor.usedBy')}
                </p>
                <ul className="flex flex-wrap gap-1">
                  {selector.used_by_privileges.map((p) => (
                    <li key={p.id}>
                      <Badge className="font-mono">{p.name}</Badge>
                    </li>
                  ))}
                </ul>
              </div>
            ) : null}
          </div>
          <div className="space-y-6">
            <SelectorTester expression={expression} onIssues={report} />
            <SelectorPreview expression={expression} onIssues={report} />
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {t('common.cancel')}
          </Button>
          <Button type="submit" loading={form.formState.isSubmitting || update.isPending}>
            {selector === null ? t('selectors.editor.saveCreate') : t('selectors.editor.save')}
          </Button>
        </DialogFooter>
      </Form>
      <UnsavedChangesGuard when={form.formState.isDirty && !finished} />
      <ConflictDialog
        {...update.conflict}
        onReload={() => {
          update.conflict.onReload()
          if (selector !== null) {
            void queryClient
              .fetchQuery({ ...selectorQuery(selector.id), staleTime: 0 })
              .then((fresh) => form.reset(valuesOf(fresh.data)))
          }
        }}
      />
    </>
  )
}
