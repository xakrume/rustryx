import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { useEffect, useState } from 'react'
import { ConflictDialog } from '@/components/conflict-dialog'
import { ErrorPanel } from '@/components/error-panel'
import { Form, useZodForm } from '@/components/form/form'
import { UnsavedChangesGuard } from '@/components/form/unsaved-changes-guard'
import { PageHeader } from '@/components/page-header'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { useSearchPatch } from '@/features/system/search-params'
import { useUpdateResource } from '@/hooks/use-update-resource'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import {
  cleanupKeys,
  createPolicy,
  fetchPolicy,
  formatsQuery,
  policyQuery,
  updatePolicy,
  type FormatInfo,
} from './api'
import {
  CRITERIA,
  EMPTY_POLICY,
  policySchema,
  toInput,
  valuesFromPolicy,
  type CleanupPolicyInput,
  type CleanupPolicyView,
  type PolicyFormValues,
} from './criteria'
import { applyPolicyErrors, BasicFields, CriteriaFields, formatLabel } from './policy-form-parts'
import { PreviewPanel } from './preview-panel'

interface PolicyFormProps {
  policy: CleanupPolicyView | undefined
  formats: readonly FormatInfo[] | undefined
  /** The format list could not be loaded: the supported criteria are unknown until the server answers on save. */
  formatsFailed: boolean
}

function PolicyForm({ policy, formats, formatsFailed }: PolicyFormProps) {
  const patchSearch = useSearchPatch()
  const queryClient = useQueryClient()
  const form = useZodForm(policySchema, {
    defaultValues: policy ? valuesFromPolicy(policy) : EMPTY_POLICY,
  })
  const format = form.watch('format')
  const supported = formats?.find((f) => f.format === format)?.cleanup_criteria ?? null
  const [cleared, setCleared] = useState(false)
  const attachedCount = policy?.attached_repositories.length ?? 0

  // A criterion the new format cannot use is emptied: it would be refused on save anyway, and a
  // hidden value in a disabled field is a trap. The note says it happened.
  const { getValues, setValue } = form
  useEffect(() => {
    if (supported === null) return
    const unsupported = CRITERIA.filter((id) => !supported.includes(id))
    const dirty = unsupported.filter((id) => getValues(id) !== '')
    dirty.forEach((id) => setValue(id, '', { shouldDirty: true }))
    setCleared(dirty.length > 0)
  }, [supported, getValues, setValue])

  const create = useMutation({
    mutationFn: (body: CleanupPolicyInput) => createPolicy(body),
    meta: { silent: true },
  })
  const update = useUpdateResource<CleanupPolicyView, CleanupPolicyInput>({
    queryKey: cleanupKeys.detail(policy?.id ?? 'new'),
    resourceName: `cleanup policy ${policy?.name ?? ''}`,
    update: (body, etag) => updatePolicy(policy?.id ?? '', body, etag),
    fetchCurrent: () => fetchPolicy(policy?.id ?? ''),
    invalidate: [cleanupKeys.all],
    copyChanges: (body) => JSON.stringify(body, null, 2),
    onSuccess: (saved) => {
      form.reset(valuesFromPolicy(saved))
      toast.success(t('cleanup.toast.saved', { name: saved.name }))
    },
    onError: (error) => {
      if (!applyPolicyErrors(form, error)) toast.apiError(error, policy?.name)
    },
  })

  async function onSubmit(values: PolicyFormValues): Promise<void> {
    const body = toInput(values)
    if (policy) {
      update.mutate(body)
      return
    }
    try {
      const created = await create.mutateAsync(body)
      // Reset before leaving so the unsaved changes guard lets the redirect through; the editor
      // then reopens on the saved policy, where the preview is available.
      form.reset(valuesFromPolicy(created))
      await queryClient.invalidateQueries({ queryKey: cleanupKeys.all })
      toast.success(t('cleanup.toast.created', { name: created.name }))
      patchSearch({ edit: created.id }, { replace: true })
    } catch (error) {
      if (!applyPolicyErrors(form, error)) toast.apiError(error)
    }
  }

  const pending = create.isPending || update.isPending
  const isDirty = form.formState.isDirty
  return (
    <div className="space-y-4">
      <UnsavedChangesGuard when={isDirty && !pending} />
      <Form
        form={form}
        onSubmit={onSubmit}
        className="max-w-3xl"
        aria-label={
          policy ? t('cleanup.editTitle', { name: policy.name }) : t('cleanup.createTitle')
        }
      >
        <BasicFields formats={formats} formatLockedBy={attachedCount} />
        {formatsFailed ? (
          <Alert variant="warning">{t('cleanup.criteria.formatsUnavailable')}</Alert>
        ) : null}
        <CriteriaFields supported={supported} formatName={formatLabel(format, formats)} />
        {cleared ? (
          <p role="status" className="text-sm text-muted-foreground">
            {t('cleanup.criteria.unsupportedCleared', { format: formatLabel(format, formats) })}
          </p>
        ) : null}
        <div className="flex flex-wrap gap-2">
          <Button type="submit" loading={pending}>
            {pending ? t('cleanup.saving') : t('cleanup.save')}
          </Button>
          <Button
            variant="outline"
            disabled={!isDirty || pending}
            onClick={() => form.reset(policy ? valuesFromPolicy(policy) : EMPTY_POLICY)}
          >
            {t('cleanup.discard')}
          </Button>
        </div>
      </Form>
      <ConflictDialog {...update.conflict} />
      {policy ? (
        <Card className="max-w-3xl">
          <CardHeader>
            <CardTitle>{t('cleanup.attached.title')}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-2">
            {policy.attached_repositories.length === 0 ? (
              <p className="text-sm text-muted-foreground">{t('cleanup.attached.none')}</p>
            ) : (
              <ul className="flex flex-wrap gap-2">
                {policy.attached_repositories.map((repo) => (
                  <li key={repo.id}>
                    <Link
                      to="/repositories/$name"
                      params={{ name: repo.name }}
                      className="inline-block rounded-md border border-border bg-muted px-2 py-0.5 font-mono text-xs text-link underline-offset-2 hover:underline"
                    >
                      {repo.name}
                    </Link>
                  </li>
                ))}
              </ul>
            )}
            <p className="text-xs text-muted-foreground">{t('cleanup.attached.note')}</p>
          </CardContent>
        </Card>
      ) : null}
      <PreviewPanel
        policyId={policy?.id}
        dirty={isDirty}
        repositories={policy?.attached_repositories.map((r) => r.name) ?? []}
      />
    </div>
  )
}

/** `/admin/cleanup-policies?edit=<id|new>`: the editor of one policy, with its dry run preview. */
export function PolicyEditorView({ policyId }: { policyId: string }) {
  const patchSearch = useSearchPatch()
  const isNew = policyId === 'new'
  const query = useQuery({ ...policyQuery(policyId), enabled: !isNew })
  const formats = useQuery(formatsQuery)
  const stored = query.data

  const title = isNew
    ? t('cleanup.createTitle')
    : stored
      ? t('cleanup.editTitle', { name: stored.data.name })
      : t('cleanup.title')

  return (
    <>
      <PageHeader
        title={title}
        description={t('cleanup.description')}
        actions={
          <Button variant="outline" onClick={() => patchSearch({ edit: undefined })}>
            {t('cleanup.backToList')}
          </Button>
        }
      />
      {!isNew && query.isError && !stored ? (
        <ErrorPanel error={query.error} onRetry={() => void query.refetch()} />
      ) : !isNew && !stored ? (
        <div aria-busy="true" className="space-y-3">
          <Skeleton className="h-10 max-w-md" />
          <Skeleton className="h-48 max-w-3xl" />
        </div>
      ) : (
        // Keyed by the ETag: a reload or a save that moved the version rebuilds the form from the
        // server's object, so it can never show a stale base.
        <PolicyForm
          key={stored?.etag ?? 'new'}
          policy={stored?.data}
          formats={formats.data?.items}
          formatsFailed={formats.isError}
        />
      )}
    </>
  )
}
