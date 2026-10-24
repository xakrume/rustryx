import '@/lib/zod-setup'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { Lock } from 'lucide-react'
import { useId } from 'react'
import { Controller, useWatch, type UseFormReturn } from 'react-hook-form'
import { z } from 'zod'
import { isApiError } from '@/api/errors'
import { ConflictDialog } from '@/components/conflict-dialog'
import { ErrorPanel } from '@/components/error-panel'
import { FormField } from '@/components/form/form-field'
import {
  applyApiFieldErrors,
  Form,
  TextareaField,
  TextField,
  useZodForm,
} from '@/components/form/form'
import { UnsavedChangesGuard } from '@/components/form/unsaved-changes-guard'
import { Alert } from '@/components/ui/alert'
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
import { Label } from '@/components/ui/label'
import { Select } from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { selectorPickerQuery } from '@/features/selectors/api'
import { useUpdateResource } from '@/hooks/use-update-resource'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { useFinish } from '@/features/roles/use-finish'
import { ActionsField } from './actions-field'
import {
  createPrivilege,
  privilegeKeys,
  privilegeQuery,
  repositoryChoicesQuery,
  REPO_FORMATS,
  updatePrivilege,
  type PrivilegeGrant,
  type PrivilegeInput,
  type PrivilegeView,
  type RepoFormat,
} from './api'

const ACTION_VALUES = ['browse', 'read', 'write', 'delete'] as const

const schema = z
  .object({
    name: z
      .string()
      .trim()
      .min(1, t('privileges.editor.nameRequired'))
      .max(128, t('privileges.editor.nameTooLong')),
    description: z.string().max(1024),
    kind: z.enum(['repository-view', 'repository-content-selector', 'admin']),
    repository: z.string(),
    format: z.string(),
    selector_id: z.string(),
    actions: z.array(z.enum(ACTION_VALUES)),
  })
  .superRefine((value, ctx) => {
    if (value.kind === 'admin') return
    if (value.repository === '') {
      ctx.addIssue({
        code: 'custom',
        path: ['repository'],
        message: t('privileges.editor.repositoryRequired'),
      })
    }
    if (value.kind === 'repository-content-selector' && value.selector_id === '') {
      ctx.addIssue({
        code: 'custom',
        path: ['selector_id'],
        message: t('privileges.editor.selectorRequired'),
      })
    }
    if (value.actions.length === 0) {
      ctx.addIssue({
        code: 'custom',
        path: ['actions'],
        message: t('privileges.editor.actionsRequired'),
      })
    }
  })
type Values = z.infer<typeof schema>

function valuesOf(privilege: PrivilegeView | null): Values {
  if (privilege === null) {
    return {
      name: '',
      description: '',
      kind: 'repository-view',
      repository: '',
      format: '',
      selector_id: '',
      actions: ['browse', 'read'],
    }
  }
  const { grant } = privilege
  return {
    name: privilege.name,
    description: privilege.description,
    kind: grant.type,
    repository: grant.type === 'admin' ? '' : grant.repository,
    format: grant.type === 'repository-view' ? (grant.format ?? '') : '',
    selector_id: grant.type === 'repository-content-selector' ? grant.selector_id : '',
    actions: [...privilege.actions],
  }
}

function inputOf(values: Values): PrivilegeInput {
  const grant: PrivilegeGrant =
    values.kind === 'repository-content-selector'
      ? {
          type: 'repository-content-selector',
          repository: values.repository,
          selector_id: values.selector_id,
        }
      : {
          type: 'repository-view',
          repository: values.repository,
          format: values.format === '' ? null : (values.format as RepoFormat),
        }
  return { name: values.name, description: values.description, grant, actions: values.actions }
}

export interface PrivilegeEditorProps {
  /** The privilege to edit, or null to create one. */
  privilegeId: string | null
  onClose: () => void
}

/**
 * Create, edit or (for a built-in privilege) read a privilege. Three grant kinds exist: admin
 * (built-in only, shown but never creatable), repository view and content selector. The actions
 * are checkboxes and at least one is required; a wildcard repository carries a visible warning.
 * Saved with `If-Match`, a lost update opens the conflict dialog.
 */
export function PrivilegeEditor({ privilegeId, onClose }: PrivilegeEditorProps) {
  const detail = useQuery({
    ...privilegeQuery(privilegeId ?? ''),
    enabled: privilegeId !== null,
    refetchOnWindowFocus: false,
  })
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-2xl">
        {privilegeId !== null && detail.isPending ? (
          <>
            <DialogHeader>
              <DialogTitle>{t('common.loading')}</DialogTitle>
              <DialogDescription className="sr-only">{t('common.loading')}</DialogDescription>
            </DialogHeader>
            <div aria-busy="true" className="space-y-3">
              <Skeleton className="h-9 w-full" />
              <Skeleton className="h-9 w-full" />
              <Skeleton className="h-24 w-full" />
            </div>
          </>
        ) : privilegeId !== null && detail.isError ? (
          <>
            <DialogHeader>
              <DialogTitle>{t('privileges.editor.loadFailed')}</DialogTitle>
              <DialogDescription className="sr-only">
                {t('privileges.editor.loadFailed')}
              </DialogDescription>
            </DialogHeader>
            <ErrorPanel
              error={detail.error}
              onRetry={() => void detail.refetch()}
              title={t('privileges.editor.loadFailed')}
            />
            <DialogFooter>
              <Button variant="outline" onClick={onClose}>
                {t('privileges.editor.close')}
              </Button>
            </DialogFooter>
          </>
        ) : (
          <PrivilegeForm privilege={detail.data?.data ?? null} onClose={onClose} />
        )}
      </DialogContent>
    </Dialog>
  )
}

function KindField({
  form,
  locked,
}: {
  form: UseFormReturn<Values, unknown, Values>
  locked: boolean
}) {
  const base = useId()
  const kind = useWatch({ control: form.control, name: 'kind' })
  if (locked) {
    return (
      <div className="space-y-1">
        <p className="text-sm font-medium">{t('privileges.editor.kind')}</p>
        <p className="text-sm">{t(`identity.grantKind.${kind}`)}</p>
        <p className="text-xs text-muted-foreground">{t('privileges.editor.kindHelper')}</p>
      </div>
    )
  }
  const options = [
    ['repository-view', 'kindRepositoryView', 'kindRepositoryViewHint', false],
    ['repository-content-selector', 'kindSelector', 'kindSelectorHint', false],
    ['admin', 'kindAdmin', 'kindAdminHint', true],
  ] as const
  return (
    <fieldset className="space-y-2">
      <legend className="text-sm font-medium">{t('privileges.editor.kind')}</legend>
      {options.map(([value, label, hint, disabled]) => (
        <div key={value} className="flex items-start gap-2">
          <input
            type="radio"
            id={`${base}-${value}`}
            name={`${base}-kind`}
            className="mt-1 size-4 accent-accent"
            checked={kind === value}
            disabled={disabled}
            aria-describedby={`${base}-${value}-hint`}
            onChange={() => form.setValue('kind', value, { shouldDirty: true })}
          />
          <div>
            <Label htmlFor={`${base}-${value}`}>{t(`privileges.editor.${label}`)}</Label>
            <p id={`${base}-${value}-hint`} className="text-xs text-muted-foreground">
              {t(`privileges.editor.${hint}`)}
            </p>
          </div>
        </div>
      ))}
    </fieldset>
  )
}

function PrivilegeForm({
  privilege,
  onClose,
}: {
  privilege: PrivilegeView | null
  onClose: () => void
}) {
  const queryClient = useQueryClient()
  const readOnly = privilege?.builtin === true
  const form = useZodForm(schema, { defaultValues: valuesOf(privilege) })
  const { finished, finish } = useFinish(onClose)
  const repositories = useQuery(repositoryChoicesQuery)
  const selectors = useQuery(selectorPickerQuery)
  const kind = useWatch({ control: form.control, name: 'kind' })
  const repository = useWatch({ control: form.control, name: 'repository' })
  const actions = useWatch({ control: form.control, name: 'actions' })
  const chosen = (repositories.data ?? []).find((r) => r.name === repository)

  const update = useUpdateResource<PrivilegeView, PrivilegeInput>({
    queryKey: privilegeKeys.detail(privilege?.id ?? ''),
    resourceName: privilege === null ? '' : `privilege ${privilege.name}`,
    update: (input, etag) => updatePrivilege(privilege!.id, input, etag),
    fetchCurrent: () => queryClient.fetchQuery({ ...privilegeQuery(privilege!.id), staleTime: 0 }),
    invalidate: [privilegeKeys.all, ['roles']],
    copyChanges: (input) => JSON.stringify(input, null, 2),
    onSuccess: (saved) => {
      toast.success(t('privileges.editor.saved', { name: saved.name }))
      finish()
    },
    onError: (error) => failed(error),
  })

  function failed(error: unknown): void {
    if (isApiError(error) && error.code === 'already_exists') {
      form.setError('name', { type: 'server', message: error.message }, { shouldFocus: true })
      return
    }
    if (!applyApiFieldErrors(form, error)) toast.apiError(error, t('privileges.editor.saveFailed'))
  }

  async function onSubmit(values: Values): Promise<void> {
    const input = inputOf(values)
    if (privilege !== null) {
      update.mutate(input)
      return
    }
    try {
      const created = await createPrivilege(input)
      void queryClient.invalidateQueries({ queryKey: privilegeKeys.all })
      toast.success(t('privileges.editor.created', { name: created.data.name }))
      finish()
    } catch (error) {
      failed(error)
    }
  }

  const title =
    privilege === null
      ? t('privileges.editor.createTitle')
      : t(readOnly ? 'privileges.editor.viewTitle' : 'privileges.editor.editTitle', {
          name: privilege.name,
        })

  return (
    <>
      <DialogHeader>
        <DialogTitle>
          {readOnly ? <Lock className="mr-2 inline size-4" aria-hidden="true" /> : null}
          {title}
        </DialogTitle>
        <DialogDescription>
          {privilege === null
            ? t('privileges.editor.createDescription')
            : t('privileges.editor.editDescription')}
        </DialogDescription>
      </DialogHeader>
      {readOnly ? <Alert className="mb-3">{t('privileges.editor.builtinNotice')}</Alert> : null}
      <Form form={form} onSubmit={onSubmit} aria-label={title}>
        <TextField<Values>
          name="name"
          label={t('privileges.editor.name')}
          required
          readOnly={readOnly}
          autoComplete="off"
          spellCheck={false}
          className="font-mono"
        />
        <TextareaField<Values>
          name="description"
          label={t('privileges.editor.descriptionLabel')}
          readOnly={readOnly}
          rows={2}
        />
        <KindField form={form} locked={privilege !== null} />

        {kind === 'admin' ? (
          <Alert>{t('privileges.summary.admin')}</Alert>
        ) : (
          <>
            <Controller
              control={form.control}
              name="repository"
              render={({ field, fieldState }) => (
                <FormField
                  label={t('privileges.editor.repository')}
                  helper={t('privileges.editor.repositoryHelper')}
                  error={fieldState.error?.message}
                  required
                >
                  {(control) => (
                    <Select
                      {...control}
                      name={field.name}
                      value={field.value}
                      disabled={readOnly}
                      onBlur={field.onBlur}
                      onChange={(e) => {
                        field.onChange(e.target.value)
                        const picked = (repositories.data ?? []).find(
                          (r) => r.name === e.target.value,
                        )
                        // A grant on one repository is always for that repository's format.
                        if (picked) form.setValue('format', picked.format, { shouldDirty: true })
                        if (e.target.value === '*')
                          form.setValue('format', '', { shouldDirty: true })
                      }}
                    >
                      <option value="">{t('common.none')}</option>
                      <option value="*">{t('privileges.editor.repositoryAll')}</option>
                      {(repositories.data ?? []).map((r) => (
                        <option key={r.name} value={r.name}>
                          {r.name} ({r.format}, {r.repo_type})
                        </option>
                      ))}
                    </Select>
                  )}
                </FormField>
              )}
            />
            {repository === '*' ? (
              <Alert variant="warning">
                <p>{t('privileges.editor.wildcardWarning')}</p>
                {actions.includes('write') || actions.includes('delete') ? (
                  <p className="mt-1 font-medium">{t('privileges.editor.wildcardWriteWarning')}</p>
                ) : null}
              </Alert>
            ) : null}
            {kind === 'repository-view' ? (
              <Controller
                control={form.control}
                name="format"
                render={({ field, fieldState }) => (
                  <FormField
                    label={t('privileges.editor.format')}
                    helper={
                      chosen
                        ? t('privileges.editor.formatFromRepository')
                        : t('privileges.editor.formatHelper')
                    }
                    error={fieldState.error?.message}
                  >
                    {(control) => (
                      <Select
                        {...control}
                        name={field.name}
                        value={field.value}
                        disabled={readOnly || chosen !== undefined}
                        onBlur={field.onBlur}
                        onChange={(e) => field.onChange(e.target.value)}
                      >
                        <option value="">{t('identity.format.any')}</option>
                        {REPO_FORMATS.map((f) => (
                          <option key={f} value={f}>
                            {f}
                          </option>
                        ))}
                      </Select>
                    )}
                  </FormField>
                )}
              />
            ) : (
              <Controller
                control={form.control}
                name="selector_id"
                render={({ field, fieldState }) => (
                  <FormField
                    label={t('privileges.editor.selector')}
                    helper={
                      (selectors.data ?? []).length === 0 && selectors.isSuccess ? (
                        <>
                          {t('privileges.editor.selectorNone')}{' '}
                          <Link to="/admin/content-selectors" className="text-link underline">
                            {t('privileges.editor.selectorCreate')}
                          </Link>
                        </>
                      ) : (
                        t('privileges.editor.selectorHelper')
                      )
                    }
                    error={fieldState.error?.message}
                    required
                  >
                    {(control) => (
                      <Select
                        {...control}
                        name={field.name}
                        value={field.value}
                        disabled={readOnly}
                        onBlur={field.onBlur}
                        onChange={(e) => field.onChange(e.target.value)}
                      >
                        <option value="">{t('common.none')}</option>
                        {(selectors.data ?? []).map((s) => (
                          <option key={s.id} value={s.id}>
                            {s.name}
                          </option>
                        ))}
                      </Select>
                    )}
                  </FormField>
                )}
              />
            )}
            <Controller
              control={form.control}
              name="actions"
              render={({ field, fieldState }) => (
                <ActionsField
                  value={field.value}
                  onChange={field.onChange}
                  error={fieldState.error?.message}
                  disabled={readOnly}
                />
              )}
            />
          </>
        )}

        {privilege !== null ? (
          <div className="space-y-1 text-sm">
            <p className="font-medium">{t('privileges.editor.usedBy')}</p>
            {privilege.roles.length === 0 ? (
              <p className="text-muted-foreground">{t('privileges.editor.usedByNone')}</p>
            ) : (
              <ul className="flex flex-wrap gap-1">
                {privilege.roles.map((r) => (
                  <li key={r.id}>
                    <Badge className="font-mono">{r.name}</Badge>
                  </li>
                ))}
              </ul>
            )}
          </div>
        ) : null}

        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {readOnly ? t('privileges.editor.close') : t('common.cancel')}
          </Button>
          {readOnly ? null : (
            <Button type="submit" loading={form.formState.isSubmitting || update.isPending}>
              {privilege === null ? t('privileges.editor.saveCreate') : t('privileges.editor.save')}
            </Button>
          )}
        </DialogFooter>
      </Form>
      <UnsavedChangesGuard when={form.formState.isDirty && !finished} />
      <ConflictDialog
        {...update.conflict}
        onReload={() => {
          update.conflict.onReload()
          if (privilege !== null) {
            void queryClient
              .fetchQuery({ ...privilegeQuery(privilege.id), staleTime: 0 })
              .then((fresh) => form.reset(valuesOf(fresh.data)))
          }
        }}
      />
    </>
  )
}
