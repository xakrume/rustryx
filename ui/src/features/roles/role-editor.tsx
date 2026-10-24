import '@/lib/zod-setup'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Lock } from 'lucide-react'
import { useMemo } from 'react'
import { Controller } from 'react-hook-form'
import { z } from 'zod'
import { type ApiError, isApiError } from '@/api/errors'
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
import { Skeleton } from '@/components/ui/skeleton'
import { useUpdateResource } from '@/hooks/use-update-resource'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { grantSummary } from '@/features/privileges/grant'
import { privilegePickerQuery } from '@/features/privileges/api'
import {
  createRole,
  effectivePrivilegesQuery,
  rolePickerQuery,
  roleKeys,
  roleQuery,
  updateRole,
  type RoleInput,
  type RoleView,
} from './api'
import { cyclePath } from './cycle'
import { useFinish } from './use-finish'
import { MultiPicker, type PickerItem } from './multi-picker'

const schema = z.object({
  name: z
    .string()
    .trim()
    .min(1, t('roles.editor.nameRequired'))
    .max(128, t('roles.editor.nameTooLong')),
  description: z.string().max(1024),
  privilege_ids: z.array(z.string()),
  nested_role_ids: z.array(z.string()),
})
type Values = z.infer<typeof schema>

/** A 409 `role_cycle` is shown at the nested roles field, not in the generic conflict dialog. */
class RoleCycleError extends Error {
  constructor(readonly apiError: ApiError) {
    super(apiError.message)
  }
}

const valuesOf = (role: RoleView | null): Values => ({
  name: role?.name ?? '',
  description: role?.description ?? '',
  privilege_ids: role?.privilege_ids ?? [],
  nested_role_ids: role?.nested_role_ids ?? [],
})

export interface RoleEditorProps {
  /** The role to edit, or null to create one. */
  roleId: string | null
  onClose: () => void
}

/**
 * Create, edit or (for a built-in role) read a role in a dialog. The role is read with its ETag
 * and written back with `If-Match`; a lost update opens the three way conflict dialog. Nested
 * roles that would form a cycle are not offered, with the chain that makes the cycle as the reason,
 * and a cycle the server finds anyway is shown at the same field.
 */
export function RoleEditor({ roleId, onClose }: RoleEditorProps) {
  const detail = useQuery({
    ...roleQuery(roleId ?? ''),
    enabled: roleId !== null,
    // The version being edited must stay the version that was read: a refetch on focus would
    // only hide a change by someone else. The ETag on save is what detects it.
    refetchOnWindowFocus: false,
  })
  const loading = roleId !== null && detail.isPending
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-3xl">
        {loading ? (
          <LoadingBody />
        ) : roleId !== null && detail.isError ? (
          <ErrorBody error={detail.error} onRetry={() => void detail.refetch()} onClose={onClose} />
        ) : (
          <RoleForm role={detail.data?.data ?? null} onClose={onClose} />
        )}
      </DialogContent>
    </Dialog>
  )
}

function LoadingBody() {
  return (
    <>
      <DialogHeader>
        <DialogTitle>{t('common.loading')}</DialogTitle>
        <DialogDescription className="sr-only">{t('common.loading')}</DialogDescription>
      </DialogHeader>
      <div aria-busy="true" className="space-y-3">
        <Skeleton className="h-9 w-full" />
        <Skeleton className="h-20 w-full" />
        <Skeleton className="h-40 w-full" />
      </div>
    </>
  )
}

function ErrorBody({
  error,
  onRetry,
  onClose,
}: {
  error: unknown
  onRetry: () => void
  onClose: () => void
}) {
  return (
    <>
      <DialogHeader>
        <DialogTitle>{t('roles.editor.loadFailed')}</DialogTitle>
        <DialogDescription className="sr-only">{t('roles.editor.loadFailed')}</DialogDescription>
      </DialogHeader>
      <ErrorPanel error={error} onRetry={onRetry} title={t('roles.editor.loadFailed')} />
      <DialogFooter>
        <Button variant="outline" onClick={onClose}>
          {t('roles.editor.close')}
        </Button>
      </DialogFooter>
    </>
  )
}

function RoleForm({ role, onClose }: { role: RoleView | null; onClose: () => void }) {
  const queryClient = useQueryClient()
  const readOnly = role?.builtin === true
  const form = useZodForm(schema, { defaultValues: valuesOf(role) })
  const { finished, finish } = useFinish(onClose)
  const privileges = useQuery(privilegePickerQuery)
  const roles = useQuery(rolePickerQuery)
  const effective = useQuery({
    ...effectivePrivilegesQuery(role?.id ?? ''),
    enabled: role !== null,
  })

  const update = useUpdateResource<RoleView, RoleInput>({
    queryKey: roleKeys.detail(role?.id ?? ''),
    resourceName: role === null ? '' : `role ${role.name}`,
    update: async (input, etag) => {
      try {
        return await updateRole(role!.id, input, etag)
      } catch (error) {
        if (isApiError(error) && error.code === 'role_cycle') throw new RoleCycleError(error)
        throw error
      }
    },
    fetchCurrent: () => queryClient.fetchQuery({ ...roleQuery(role!.id), staleTime: 0 }),
    invalidate: [roleKeys.all, ['users'], ['privileges']],
    copyChanges: (input) => JSON.stringify(input, null, 2),
    onSuccess: (saved) => {
      toast.success(t('roles.editor.saved', { name: saved.name }))
      finish()
    },
    onError: (error) => failed(error),
  })

  function failed(error: unknown): void {
    if (error instanceof RoleCycleError) {
      form.setError('nested_role_ids', { type: 'server', message: t('roles.editor.cycleError') })
      return
    }
    if (isApiError(error) && error.code === 'already_exists') {
      form.setError('name', { type: 'server', message: error.message }, { shouldFocus: true })
      return
    }
    if (!applyApiFieldErrors(form, error)) toast.apiError(error, t('roles.editor.saveFailed'))
  }

  async function onSubmit(values: Values): Promise<void> {
    const input: RoleInput = { ...values }
    if (role !== null) {
      update.mutate(input)
      return
    }
    try {
      const created = await createRole(input)
      void queryClient.invalidateQueries({ queryKey: roleKeys.all })
      toast.success(t('roles.editor.created', { name: created.data.name }))
      finish()
    } catch (error) {
      failed(error)
    }
  }

  const privilegeItems = useMemo<PickerItem[]>(
    () =>
      (privileges.data ?? []).map((p) => ({
        id: p.id,
        label: p.name,
        description: grantSummary(p),
        badge: p.builtin ? <Badge>{t('identity.builtin')}</Badge> : undefined,
      })),
    [privileges.data],
  )
  const roleItems = useMemo<PickerItem[]>(
    () =>
      (roles.data ?? [])
        .filter((r) => r.id !== role?.id)
        .map((r) => {
          const path = role === null ? null : cyclePath(roles.data ?? [], role.id, r.id)
          return {
            id: r.id,
            label: r.name,
            description: r.description || undefined,
            badge: r.builtin ? <Badge>{t('identity.builtin')}</Badge> : undefined,
            disabledReason:
              path === null
                ? undefined
                : t('roles.editor.cycle', { path: path.join(' includes ') }),
          }
        }),
    [roles.data, role],
  )

  const title =
    role === null
      ? t('roles.editor.createTitle')
      : t(readOnly ? 'roles.editor.viewTitle' : 'roles.editor.editTitle', { name: role.name })
  const usage = role !== null && role.assigned_users + role.mapped_groups > 0

  return (
    <>
      <DialogHeader>
        <DialogTitle>
          {readOnly ? <Lock className="mr-2 inline size-4" aria-hidden="true" /> : null}
          {title}
        </DialogTitle>
        <DialogDescription>
          {role === null ? t('roles.editor.createDescription') : t('roles.editor.editDescription')}
        </DialogDescription>
      </DialogHeader>
      {readOnly ? <Alert className="mb-3">{t('roles.editor.builtinNotice')}</Alert> : null}
      {usage ? (
        <p className="mb-3 text-sm text-muted-foreground">
          {t('roles.editor.usedBy', {
            users: t('roles.editor.users', { count: role.assigned_users }),
            groups: t('roles.editor.groups', { count: role.mapped_groups }),
          })}
        </p>
      ) : null}
      <Form form={form} onSubmit={onSubmit} aria-label={title}>
        <TextField<Values>
          name="name"
          label={t('roles.editor.name')}
          required
          readOnly={readOnly}
          autoComplete="off"
          spellCheck={false}
          className="font-mono"
        />
        <TextareaField<Values>
          name="description"
          label={t('roles.editor.descriptionLabel')}
          readOnly={readOnly}
          rows={2}
        />
        <div className="grid gap-4 md:grid-cols-2">
          <Controller
            control={form.control}
            name="privilege_ids"
            render={({ field, fieldState }) => (
              <MultiPicker
                legend={t('roles.editor.privileges')}
                what={t('roles.editor.privilegesWhat')}
                items={privilegeItems}
                value={field.value}
                onChange={field.onChange}
                helper={t('roles.editor.privilegesHelper')}
                error={fieldState.error?.message}
                disabled={readOnly}
              />
            )}
          />
          <Controller
            control={form.control}
            name="nested_role_ids"
            render={({ field, fieldState }) => (
              <MultiPicker
                legend={t('roles.editor.nested')}
                what={t('roles.editor.nestedWhat')}
                items={roleItems}
                value={field.value}
                onChange={field.onChange}
                helper={t('roles.editor.nestedHelper')}
                error={fieldState.error?.message}
                disabled={readOnly}
              />
            )}
          />
        </div>
        {role !== null ? (
          <details open={readOnly} className="rounded-md border border-border p-3">
            <summary className="cursor-pointer text-sm font-medium">
              {t('roles.editor.effective')}
            </summary>
            <p className="mt-1 text-xs text-muted-foreground">
              {t('roles.editor.effectiveHelper')}
            </p>
            {effective.isError ? (
              <ErrorPanel
                className="mt-2"
                error={effective.error}
                onRetry={() => void effective.refetch()}
              />
            ) : (effective.data ?? []).length === 0 ? (
              <p className="mt-2 text-sm text-muted-foreground">
                {effective.isPending ? t('common.loading') : t('roles.editor.effectiveNone')}
              </p>
            ) : (
              <ul className="mt-2 space-y-1 text-sm">
                {(effective.data ?? []).map((p) => (
                  <li key={p.id}>
                    <span className="font-mono">{p.name}</span>{' '}
                    <span className="text-xs text-muted-foreground">{grantSummary(p)}</span>
                  </li>
                ))}
              </ul>
            )}
          </details>
        ) : null}
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {readOnly ? t('roles.editor.close') : t('common.cancel')}
          </Button>
          {readOnly ? null : (
            <Button type="submit" loading={form.formState.isSubmitting || update.isPending}>
              {role === null ? t('roles.editor.saveCreate') : t('roles.editor.save')}
            </Button>
          )}
        </DialogFooter>
      </Form>
      <UnsavedChangesGuard when={form.formState.isDirty && !finished} />
      <ConflictDialog
        {...update.conflict}
        onReload={() => {
          update.conflict.onReload()
          if (role !== null) {
            void queryClient
              .fetchQuery({ ...roleQuery(role.id), staleTime: 0 })
              .then((fresh) => form.reset(valuesOf(fresh.data)))
          }
        }}
      />
    </>
  )
}
