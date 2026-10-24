import { useQuery } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { Check, ChevronLeft, ChevronRight } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { ErrorPanel } from '@/components/error-panel'
import { Form, useZodForm } from '@/components/form/form'
import { UnsavedChangesGuard } from '@/components/form/unsaved-changes-guard'
import { PageHeader } from '@/components/page-header'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { blobStoresQuery, formatsQuery, useCreateRepository } from './api'
import { RepositoryFields, useFormatInfo } from './form/fields'
import { IdentityStep, useNameCheck } from './form/identity-step'
import { ReviewStep } from './form/review-step'
import {
  applyServerErrors,
  defaultValues,
  repositoryFormSchema,
  toCreateRequest,
  type RepositoryFormValues,
} from './form/values'

type Step = 1 | 2 | 3
const STEPS: readonly Step[] = [1, 2, 3]

function StepList({ step }: { step: Step }) {
  return (
    <ol
      aria-label={t('repoForm.steps.label')}
      className="m-0 flex list-none flex-wrap gap-x-6 gap-y-2 p-0"
    >
      {STEPS.map((n) => {
        const done = n < step
        return (
          <li
            key={n}
            aria-current={n === step ? 'step' : undefined}
            className={`flex items-center gap-2 text-sm ${n === step ? 'font-semibold text-foreground' : 'text-muted-foreground'}`}
          >
            <span
              aria-hidden="true"
              className={`flex size-6 items-center justify-center rounded-full border text-xs ${
                n === step
                  ? 'border-accent bg-accent text-on-accent'
                  : done
                    ? 'border-success text-success-text'
                    : 'border-input'
              }`}
            >
              {done ? <Check className="size-3.5" /> : n}
            </span>
            <span>
              {t(`repoForm.steps.${n}`)}
              {done ? <span className="sr-only"> ({t('repoForm.steps.done')})</span> : null}
            </span>
          </li>
        )
      })}
    </ol>
  )
}

/**
 * The create wizard (docs/ui-page-inventory.md, 7.2): format and type, then the settings that
 * type needs, then a review that shows what will be sent. One form holds all three steps, so
 * going back loses nothing; each step validates its own fields before it lets you move on. A
 * failed create puts the server's field errors on the fields and returns to the step that holds
 * the first of them.
 */
export function CreateRepositoryPage() {
  const formats = useQuery(formatsQuery)
  const blobStores = useQuery(blobStoresQuery)
  return (
    <>
      <PageHeader title={t('pages.createRepository')} description={t('repoForm.description')} />
      {formats.error ? (
        <ErrorPanel
          error={formats.error}
          title={t('repoForm.loadFailed')}
          onRetry={() => void formats.refetch()}
        />
      ) : formats.data === undefined ? (
        <div aria-busy="true" className="space-y-3">
          <Skeleton className="h-6 w-80" />
          <Skeleton className="h-64" />
        </div>
      ) : (
        <Wizard firstBlobStoreId={blobStores.data?.items[0]?.id ?? ''} />
      )}
    </>
  )
}

function Wizard({ firstBlobStoreId }: { firstBlobStoreId: string }) {
  const navigate = useNavigate()
  const [step, setStep] = useState<Step>(1)
  const [created, setCreated] = useState<string | null>(null)
  const [submitError, setSubmitError] = useState<unknown>(null)
  const create = useCreateRepository()
  const form = useZodForm<RepositoryFormValues>(repositoryFormSchema('create'), {
    defaultValues: defaultValues([]),
  })
  const { setValue, setFocus, getValues, trigger, watch } = form
  const name = watch('name')
  const format = watch('format')
  const { fields } = useFormatInfo(format)
  const check = useNameCheck(name)
  const refocus = useRef(false)

  // The first blob store is the default choice. It is not a user edit, so it is not "dirty".
  const blobStoreId = watch('blob_store_id')
  useEffect(() => {
    if (blobStoreId === '' && firstBlobStoreId !== '') setValue('blob_store_id', firstBlobStoreId)
  }, [blobStoreId, firstBlobStoreId, setValue])

  // After a server error sends the wizard back to an earlier step, focus the field once it is there.
  useEffect(() => {
    if (!refocus.current) return
    refocus.current = false
    const first = Object.keys(form.formState.errors)[0]
    if (first) setFocus(first as keyof RepositoryFormValues)
  }, [step, form.formState.errors, setFocus])

  // Leave for the new repository only after the form is clean again, or the guard blocks it.
  useEffect(() => {
    if (created !== null) {
      void navigate({ to: '/repositories/$name', params: { name: created } })
    }
  }, [created, navigate])

  async function next(): Promise<void> {
    if (step === 1) {
      const ok = await trigger(['name', 'format', 'repo_type'])
      if (ok && !check.taken) setStep(2)
      else if (check.taken) setFocus('name')
      return
    }
    // The schema checks the whole form; step 2 shows only some of the fields, but a type specific
    // rule (a proxy needs a URL) only fires for the fields that type shows.
    const ok = await trigger()
    if (ok) setStep(3)
  }

  async function submit(values: RepositoryFormValues): Promise<void> {
    setSubmitError(null)
    try {
      const result = await create.mutateAsync(toCreateRequest(values, fields))
      toast.success(t('repoForm.created', { name: result.data.name }))
      form.reset(values)
      setCreated(result.data.name)
    } catch (error) {
      const applied = applyServerErrors(form, error)
      if (applied.applied) {
        refocus.current = true
        setStep(applied.step === 1 ? 1 : 2)
      } else {
        setSubmitError(error)
      }
    }
  }

  return (
    <Form
      form={form}
      onSubmit={submit}
      className="space-y-5"
      aria-label={t('pages.createRepository')}
      onKeyDown={(event) => {
        // Enter in a field moves to the next step; only the last step creates the repository.
        const target = event.target as HTMLElement
        if (event.key === 'Enter' && step < 3 && target.tagName === 'INPUT') {
          event.preventDefault()
          void next()
        }
      }}
    >
      <UnsavedChangesGuard when={form.formState.isDirty && created === null} />
      <StepList step={step} />
      {step === 1 ? <IdentityStep /> : null}
      {step === 2 ? <RepositoryFields mode="create" self={null} hasStoredAuth={false} /> : null}
      {step === 3 ? (
        <>
          <ReviewStep values={getValues()} fields={fields} />
          {submitError !== null ? (
            <ErrorPanel error={submitError} title={t('repoForm.createFailed')} />
          ) : null}
        </>
      ) : null}
      <div className="flex flex-wrap items-center justify-between gap-2 border-t border-border pt-4">
        {step > 1 ? (
          <Button variant="outline" onClick={() => setStep((s) => (s - 1) as Step)}>
            <ChevronLeft aria-hidden="true" />
            {t('common.back')}
          </Button>
        ) : (
          <span />
        )}
        {step < 3 ? (
          <Button onClick={() => void next()}>
            {t('repoForm.next')}
            <ChevronRight aria-hidden="true" />
          </Button>
        ) : (
          <Button type="submit" loading={create.isPending}>
            {t('repos.create')}
          </Button>
        )}
      </div>
    </Form>
  )
}
