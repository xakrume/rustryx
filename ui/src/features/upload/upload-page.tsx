import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useNavigate, useSearch } from '@tanstack/react-router'
import { Upload } from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { isAbortError, isApiError } from '@/api/errors'
import { EmptyState } from '@/components/empty-state'
import { ErrorPanel } from '@/components/error-panel'
import { FormField } from '@/components/form/form-field'
import { PageHeader } from '@/components/page-header'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Select } from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { ClientSetupPanel } from '@/features/client-setup/client-setup-panel'
import { componentKeys } from '@/features/components/api'
import {
  repositoryKeys,
  repositoryNamesQuery,
  type RepositoryView,
} from '@/features/repositories/api'
import { formatLabel } from '@/features/repositories/repo-meta'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { uploadSchemaQuery } from './api'
import { UploadForm } from './upload-form'
import {
  buildAttributes,
  fileProgress,
  hasErrors,
  initialFields,
  mapServerErrors,
  noErrors,
  validate,
  type UploadErrors,
  type UploadFormState,
  type UploadSchema,
} from './upload-model'
import {
  newIdempotencyKey,
  startUpload,
  type UploadHandle,
  type UploadResult,
} from './upload-client'
import { ProgressPanel, ResultPanel, UploadErrorPanel } from './upload-status'

type Phase =
  | { name: 'editing'; error: unknown; cancelled: boolean }
  | { name: 'uploading'; progress: number[] }
  | { name: 'done'; result: UploadResult }

const EDITING: Phase = { name: 'editing', error: null, cancelled: false }

/** The first field with a problem gets the focus; a problem that is not on a field is announced as an alert. */
/** The first invalid field, else the first message: an input that can take focus is preferred. */
function focusFirstProblem(root: HTMLElement | null): void {
  const invalid = root?.querySelector<HTMLElement>('[aria-invalid="true"]')
  const target = invalid ?? root?.querySelector<HTMLElement>('[role="alert"]')
  target?.scrollIntoView?.({ block: 'center' })
  if (target?.matches('input, select, textarea')) target.focus()
}

function UploadSection({
  repository,
  schema,
}: {
  repository: RepositoryView
  schema: UploadSchema
}) {
  const queryClient = useQueryClient()
  const rootRef = useRef<HTMLFormElement | null>(null)
  const handleRef = useRef<UploadHandle | null>(null)
  const [state, setState] = useState<UploadFormState>(() => ({
    fields: initialFields(schema),
    files: [],
    assetFields: {},
  }))
  const [errors, setErrors] = useState<UploadErrors>(noErrors)
  const [phase, setPhase] = useState<Phase>(EDITING)
  // The key says "this is the same upload": a retry of an unchanged form replays the first answer
  // if the server did commit it. Any edit makes a different upload, so it gets a new key.
  const [key, setKey] = useState(newIdempotencyKey)
  // Bumped together with the errors, so the focus moves after React rendered them.
  const [problemTick, setProblemTick] = useState(0)
  useEffect(() => {
    if (problemTick > 0) focusFirstProblem(rootRef.current)
  }, [problemTick])

  useEffect(() => () => handleRef.current?.abort(), [])

  const change = useCallback((next: UploadFormState) => {
    setState(next)
    setKey(newIdempotencyKey())
    setErrors(noErrors)
  }, [])

  const run = useCallback(
    async (current: UploadFormState, idempotencyKey: string): Promise<void> => {
      const sizes = current.files.map((f) => f.file.size)
      setPhase({ name: 'uploading', progress: sizes.map(() => 0) })
      const handle = startUpload(
        {
          repository: repository.name,
          attributes: buildAttributes(schema, current),
          files: current.files.map((f) => f.file),
          idempotencyKey,
        },
        (loaded, total) =>
          setPhase({ name: 'uploading', progress: fileProgress(sizes, loaded, total, loaded) }),
      )
      handleRef.current = handle
      try {
        const result = await handle.promise
        setPhase({ name: 'done', result })
        toast.success(t('upload.done', { name: repository.name }))
        void queryClient.invalidateQueries({ queryKey: componentKeys.all })
        void queryClient.invalidateQueries({ queryKey: repositoryKeys.all })
        void queryClient.invalidateQueries({ queryKey: ['browse'] })
        void queryClient.invalidateQueries({ queryKey: ['search'] })
      } catch (error) {
        if (isAbortError(error)) {
          setPhase({ name: 'editing', error: null, cancelled: true })
          return
        }
        if (isApiError(error) && error.fieldErrors.length > 0) {
          setErrors(mapServerErrors(error.fieldErrors, current.files))
          setProblemTick((tick) => tick + 1)
        }
        setPhase({ name: 'editing', error, cancelled: false })
      } finally {
        handleRef.current = null
      }
    },
    [queryClient, repository.name, schema],
  )

  function submit(event: React.FormEvent): void {
    event.preventDefault()
    const found = validate(schema, state)
    if (hasErrors(found)) {
      setErrors(found)
      setProblemTick((tick) => tick + 1)
      return
    }
    setErrors(noErrors)
    void run(state, key)
  }

  if (phase.name === 'done') {
    return (
      <ResultPanel
        result={phase.result}
        onAnother={() => {
          setState({ fields: initialFields(schema), files: [], assetFields: {} })
          setKey(newIdempotencyKey())
          setPhase(EDITING)
        }}
      />
    )
  }

  const uploading = phase.name === 'uploading'
  return (
    <form
      ref={rootRef}
      noValidate
      onSubmit={submit}
      className="space-y-4"
      aria-label={t('nav.items.upload')}
    >
      {phase.name === 'editing' && phase.cancelled ? (
        <Alert variant="info">{t('upload.cancelled')}</Alert>
      ) : null}
      {phase.name === 'editing' && phase.error !== null ? (
        <UploadErrorPanel error={phase.error} onRetry={() => void run(state, key)} />
      ) : null}
      {uploading ? (
        <ProgressPanel
          files={state.files}
          progress={phase.progress}
          onCancel={() => handleRef.current?.abort()}
        />
      ) : null}
      <fieldset disabled={uploading} className="m-0 min-w-0 border-0 p-0">
        <UploadForm
          schema={schema}
          state={state}
          errors={errors}
          disabled={uploading}
          onChange={change}
        />
      </fieldset>
      <div className="flex items-center gap-2 pt-2">
        <Button type="submit" loading={uploading}>
          <Upload aria-hidden="true" />
          {uploading ? t('upload.uploading') : t('upload.submit')}
        </Button>
        {repository.online ? null : (
          <span className="text-sm text-warning-text">{t('upload.offlineNote')}</span>
        )}
      </div>
    </form>
  )
}

function Target({ repository }: { repository: RepositoryView }) {
  const schema = useQuery(uploadSchemaQuery(repository.format))
  if (schema.error) {
    return (
      <ErrorPanel
        error={schema.error}
        title={t('upload.schemaFailed')}
        onRetry={() => void schema.refetch()}
      />
    )
  }
  if (schema.data === undefined) {
    return (
      <div aria-busy="true" className="space-y-3">
        <Skeleton className="h-40" />
        <Skeleton className="h-40" />
      </div>
    )
  }
  if (!schema.data.uploadable) {
    return (
      <div className="space-y-4">
        <Alert
          variant="info"
          title={t('upload.notUploadableTitle', { format: formatLabel(repository.format) })}
        >
          {schema.data.reason ?? t('upload.notUploadableBody')} {t('upload.useClient')}
        </Alert>
        <ClientSetupPanel repository={repository.name} />
      </div>
    )
  }
  return <UploadSection key={repository.name} repository={repository} schema={schema.data} />
}

/**
 * `/upload`: choose a hosted repository you may write to, then fill the form that its format
 * defines. The form is built from the upload schema of the server, so a new format needs no
 * change here. A repository that cannot take uploads from the browser (docker, OCI, or one you
 * only read) says why and shows how to publish with its own client instead.
 */
export function UploadPage() {
  const search = useSearch({ from: '/_app/upload' })
  const navigate = useNavigate()
  const names = useQuery(repositoryNamesQuery)
  const items = useMemo(() => names.data?.items ?? [], [names.data])
  const targets = useMemo(
    () => items.filter((r) => r.repo_type === 'hosted' && r.permissions.write),
    [items],
  )
  const selected = search.repository ?? ''
  const repository = items.find((r) => r.name === selected)
  const eligible = targets.some((r) => r.name === selected)

  return (
    <>
      <PageHeader title={t('nav.items.upload')} description={t('upload.description')} />
      {names.error ? (
        <ErrorPanel
          error={names.error}
          title={t('upload.reposFailed')}
          onRetry={() => void names.refetch()}
        />
      ) : names.data === undefined ? (
        <div aria-busy="true">
          <Skeleton className="h-16 max-w-md" />
        </div>
      ) : targets.length === 0 ? (
        <EmptyState
          icon={Upload}
          title={t('upload.noTargetsTitle')}
          description={t('upload.noTargetsBody')}
        />
      ) : (
        <div className="space-y-4">
          <FormField
            label={t('upload.repository')}
            helper={t('upload.repositoryHelp')}
            className="max-w-md"
          >
            {(control) => (
              <Select
                {...control}
                value={selected}
                onChange={(event) =>
                  void navigate({
                    to: '/upload',
                    replace: true,
                    search: {
                      repository: event.target.value === '' ? undefined : event.target.value,
                    },
                  })
                }
              >
                <option value="">{t('upload.chooseRepository')}</option>
                {targets.map((r) => (
                  <option key={r.id} value={r.name}>
                    {r.name} ({formatLabel(r.format)})
                  </option>
                ))}
              </Select>
            )}
          </FormField>
          {repository !== undefined && !eligible ? (
            <div className="space-y-4">
              <Alert variant="warning" title={t('upload.cannotTitle', { name: repository.name })}>
                {repository.repo_type !== 'hosted'
                  ? t('upload.cannotNotHosted')
                  : t('upload.cannotNoWrite')}
              </Alert>
              <ClientSetupPanel repository={repository.name} />
            </div>
          ) : repository !== undefined ? (
            <Target repository={repository} />
          ) : selected !== '' ? (
            <Alert variant="warning">{t('upload.unknownRepository', { name: selected })}</Alert>
          ) : null}
        </div>
      )}
    </>
  )
}
