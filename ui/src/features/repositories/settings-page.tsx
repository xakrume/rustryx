import { useQuery } from '@tanstack/react-query'
import { useNavigate, useParams } from '@tanstack/react-router'
import { Lock } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { clientSetupKeys } from '@/features/client-setup/api'
import { ConflictDialog } from '@/components/conflict-dialog'
import { ErrorPanel } from '@/components/error-panel'
import { Form, useZodForm } from '@/components/form/form'
import { UnsavedChangesGuard } from '@/components/form/unsaved-changes-guard'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { RelativeTime } from '@/components/relative-time'
import { useUpdateResource } from '@/hooks/use-update-resource'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import {
  fetchRepository,
  patchRepository,
  repositoryKeys,
  repositoryQuery,
  useRepositoryAction,
  type RepositoryView,
} from './api'
import { DeleteRepositoryDialog } from './delete-repository-dialog'
import { RepositoryFields, useFormatInfo } from './form/fields'
import {
  applyServerErrors,
  repositoryFormSchema,
  toPatch,
  valuesFromRepository,
  type RepositoryFormValues,
} from './form/values'
import { FormatBadge, RepoTypeBadge, UpstreamBadge } from './repo-meta'

function Locked({ repo }: { repo: RepositoryView }) {
  return (
    <section className="space-y-2 rounded-lg border border-border bg-card p-4">
      <h2 className="flex items-center gap-2 text-base font-semibold">
        <Lock className="size-4 text-muted-foreground" aria-hidden="true" />
        {t('repoForm.locked.title')}
      </h2>
      <p className="text-sm text-muted-foreground">{t('repoForm.locked.body')}</p>
      <dl className="grid gap-x-6 gap-y-1 text-sm sm:grid-cols-[auto_1fr]">
        <dt className="text-muted-foreground">{t('repoForm.identity.name')}</dt>
        <dd className="font-mono">{repo.name}</dd>
        <dt className="text-muted-foreground">{t('repoForm.identity.format')}</dt>
        <dd>
          <FormatBadge format={repo.format} />
        </dd>
        <dt className="text-muted-foreground">{t('repoForm.identity.type')}</dt>
        <dd>
          <RepoTypeBadge type={repo.repo_type} />
        </dd>
      </dl>
    </section>
  )
}

function UpstreamActions({ repo }: { repo: RepositoryView }) {
  const actions = useRepositoryAction(repo.name)
  const upstream = repo.upstream
  const blocked = upstream?.state === 'blocked'
  return (
    <section className="space-y-3 rounded-lg border border-border bg-card p-4">
      <h2 className="text-base font-semibold">{t('repoForm.upstream.title')}</h2>
      {upstream ? (
        <p className="flex flex-wrap items-center gap-2 text-sm">
          <UpstreamBadge state={upstream.state} />
          {upstream.last_error_code ? (
            <span>
              {t('repoForm.upstream.lastError')}{' '}
              <code className="font-mono">{upstream.last_error_code}</code>
            </span>
          ) : null}
          <span className="text-muted-foreground">
            {t('repoForm.upstream.checked')} <RelativeTime value={upstream.last_checked_at} />
          </span>
        </p>
      ) : null}
      <p className="text-sm text-muted-foreground">{t('repoForm.upstream.body')}</p>
      <div className="flex flex-wrap gap-2">
        <Button
          variant="outline"
          loading={actions.invalidateCache.isPending}
          onClick={() =>
            actions.invalidateCache.mutate(undefined, {
              onSuccess: () =>
                toast.success(t('repoForm.upstream.invalidated', { name: repo.name })),
              onError: (error) => toast.apiError(error, t('repoForm.upstream.invalidateFailed')),
            })
          }
        >
          {t('repoForm.upstream.invalidate')}
        </Button>
        <Button
          variant="outline"
          loading={actions.setBlocked.isPending}
          onClick={() =>
            actions.setBlocked.mutate(!blocked, {
              onSuccess: () =>
                toast.success(
                  blocked
                    ? t('repoForm.upstream.unblocked', { name: repo.name })
                    : t('repoForm.upstream.blocked', { name: repo.name }),
                ),
              onError: (error) => toast.apiError(error, t('repoForm.upstream.blockFailed')),
            })
          }
        >
          {blocked ? t('repoForm.upstream.unblock') : t('repoForm.upstream.block')}
        </Button>
      </div>
    </section>
  )
}

function DangerZone({ repo }: { repo: RepositoryView }) {
  const [open, setOpen] = useState(false)
  const navigate = useNavigate()
  return (
    <section className="space-y-2 rounded-lg border border-destructive bg-card p-4">
      <h2 className="text-base font-semibold text-destructive-text">
        {t('repoForm.danger.title')}
      </h2>
      <p className="text-sm text-muted-foreground">{t('repoForm.danger.body')}</p>
      <Button variant="destructive" onClick={() => setOpen(true)}>
        {t('repoForm.danger.delete')}
      </Button>
      <DeleteRepositoryDialog
        name={open ? repo.name : null}
        onClose={() => setOpen(false)}
        onDeleted={() => void navigate({ to: '/repositories' })}
      />
    </section>
  )
}

/**
 * Repository settings: the same form as the create wizard (step 2), pre-filled, with the fields
 * that cannot change after creation locked and explained. Saving sends a merge patch of the
 * changed fields with the ETag that was read (`If-Match`); a 412 opens the three way dialog
 * and a 409 (a group cycle) names the conflict. Leaving with unsaved edits asks first.
 */
export function RepositorySettingsPage() {
  const { name } = useParams({ from: '/_app/repositories/$name/settings' })
  const query = useQuery(repositoryQuery(name))
  const repo = query.data?.data
  const { info, fields } = useFormatInfo(repo?.format ?? '')

  if (repo === undefined || info === undefined) {
    return query.error ? (
      <ErrorPanel error={query.error} onRetry={() => void query.refetch()} />
    ) : (
      <div aria-busy="true" className="space-y-3">
        <Skeleton className="h-32" />
        <Skeleton className="h-48" />
      </div>
    )
  }
  return <SettingsForm repo={repo} etag={query.data?.etag ?? null} fields={fields} />
}

function SettingsForm({
  repo,
  etag,
  fields,
}: {
  repo: RepositoryView
  etag: string | null
  fields: ReturnType<typeof useFormatInfo>['fields']
}) {
  const initial = useMemo(() => valuesFromRepository(repo, fields), [repo, fields])
  const initialRef = useRef(initial)
  const form = useZodForm<RepositoryFormValues>(repositoryFormSchema('edit'), {
    defaultValues: initial,
  })
  const { reset } = form
  const isDirty = form.formState.isDirty

  // A new server version (after a save, a reload, or another tab) replaces the form values,
  // unless the user is mid edit: then their edit stays until they save, discard or reload.
  useEffect(() => {
    initialRef.current = initial
    if (!form.formState.isDirty) reset(initial)
    // `etag` is the version marker: a refetch with the same content must not reset a dirty form.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [etag])

  const update = useUpdateResource<RepositoryView, RepositoryFormValues>({
    queryKey: repositoryKeys.detail(repo.name),
    resourceName: t('repos.resource', { name: repo.name }),
    update: (values, currentEtag) =>
      patchRepository(repo.name, toPatch(values, initialRef.current, fields), currentEtag),
    fetchCurrent: () => fetchRepository(repo.name),
    invalidate: [repositoryKeys.all, clientSetupKeys.all],
    copyChanges: (values) => JSON.stringify(toPatch(values, initialRef.current, fields), null, 2),
    onSuccess: (saved) => {
      reset(valuesFromRepository(saved, fields))
      toast.success(t('repoForm.saved', { name: saved.name }))
    },
    onError: (error) => {
      if (!applyServerErrors(form, error).applied) {
        toast.apiError(error, t('repoForm.saveFailed'))
      }
    },
  })

  function save(values: RepositoryFormValues): void {
    const patch = toPatch(values, initialRef.current, fields)
    if (Object.keys(patch).length === 0) {
      toast.info(t('repoForm.noChanges'))
      return
    }
    update.mutate(values)
  }

  return (
    <div className="space-y-4">
      <h2 className="sr-only">{t('repos.tabs.settings')}</h2>
      <Form
        form={form}
        onSubmit={save}
        aria-label={t('repoForm.formLabel', { name: repo.name })}
        className="space-y-4"
      >
        <UnsavedChangesGuard when={isDirty} />
        <Locked repo={repo} />
        <RepositoryFields
          mode="edit"
          self={repo.name}
          hasStoredAuth={repo.proxy?.auth != null}
          blobStoreName={repo.blob_store.name}
        />
        <div className="flex flex-wrap items-center gap-2 border-t border-border pt-4">
          <Button type="submit" disabled={!isDirty} loading={update.isPending}>
            {t('common.save')}
          </Button>
          <Button
            variant="outline"
            disabled={!isDirty || update.isPending}
            onClick={() => reset(initial)}
          >
            {t('repoForm.discard')}
          </Button>
          {!isDirty ? (
            <span className="text-sm text-muted-foreground">{t('repoForm.nothingToSave')}</span>
          ) : null}
        </div>
      </Form>
      {repo.repo_type === 'proxy' ? <UpstreamActions repo={repo} /> : null}
      <DangerZone repo={repo} />
      <ConflictDialog
        {...update.conflict}
        onReload={() => {
          // "Reload" means: drop my edit and show what the server has now.
          reset(initialRef.current)
          update.conflict.onReload()
        }}
      />
      {repo.permissions.admin ? null : <Alert variant="warning">{t('repoForm.readOnly')}</Alert>}
    </div>
  )
}
