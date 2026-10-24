import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { ConflictDialog } from '@/components/conflict-dialog'
import { ErrorPanel } from '@/components/error-panel'
import { Alert } from '@/components/ui/alert'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { ActionConfirmDialog } from '@/features/roles/action-confirm-dialog'
import { useUpdateResource } from '@/hooks/use-update-resource'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { anonymousKeys, anonymousQuery, updateGlobal, type AnonymousAccess } from './api'

/**
 * The global anonymous switch. In this version it is read only (`global_editable` false: the value
 * comes from the configuration file), and the card says so and why instead of showing a dead
 * control without a reason. When the server makes it editable, turning it on asks first, naming the
 * exposure, and the change is sent with `If-Match`.
 */
export function GlobalSwitchCard() {
  const queryClient = useQueryClient()
  const query = useQuery(anonymousQuery)
  const [pending, setPending] = useState<boolean | null>(null)
  const update = useUpdateResource<AnonymousAccess, boolean>({
    queryKey: anonymousKeys.state(),
    resourceName: t('anonymous.global.title'),
    update: (enabled, etag) => updateGlobal(enabled, etag),
    fetchCurrent: () => queryClient.fetchQuery({ ...anonymousQuery, staleTime: 0 }),
    invalidate: [anonymousKeys.all, ['repositories']],
    onSuccess: (saved) =>
      toast.success(
        t('anonymous.global.saved', {
          state: saved.global_enabled ? t('anonymous.global.on') : t('anonymous.global.off'),
        }),
      ),
    onError: (error) => toast.apiError(error, t('anonymous.global.failed')),
  })

  if (query.isPending) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>{t('anonymous.global.title')}</CardTitle>
        </CardHeader>
        <CardContent aria-busy="true">
          <Skeleton className="h-20 w-full" />
        </CardContent>
      </Card>
    )
  }
  if (query.isError) {
    return (
      <ErrorPanel
        error={query.error}
        onRetry={() => void query.refetch()}
        title={t('identity.loadFailed')}
      />
    )
  }

  const state = query.data.data
  const enabling = pending === true
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('anonymous.global.title')}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="flex items-center justify-between gap-3">
          <Label htmlFor="anonymous-global">{t('anonymous.global.label')}</Label>
          <span className="flex items-center gap-2 text-sm">
            <span>
              {state.global_enabled ? t('anonymous.global.on') : t('anonymous.global.off')}
            </span>
            <Switch
              id="anonymous-global"
              checked={state.global_enabled}
              disabled={!state.global_editable}
              onCheckedChange={(checked) => setPending(checked)}
            />
          </span>
        </div>
        {!state.global_editable ? (
          <Alert title={t('anonymous.global.readOnlyTitle')}>
            {t('anonymous.global.readOnlyBody')}
          </Alert>
        ) : null}
        {!state.global_enabled ? (
          <p className="text-sm text-muted-foreground">{t('anonymous.global.offExplain')}</p>
        ) : null}
        <p className="text-sm">
          <span className="text-muted-foreground">{t('anonymous.global.role')}</span>{' '}
          <span className="font-mono">{state.anonymous_role.name}</span>
        </p>
        <ul className="grid gap-1 text-sm">
          <li>{t('anonymous.global.publicCount', { count: state.public_repository_count })}</li>
          <li>
            {t('anonymous.global.effectiveCount', { count: state.effective_repository_count })}
          </li>
        </ul>
      </CardContent>
      <ActionConfirmDialog
        open={pending !== null}
        onOpenChange={(open) => !open && setPending(null)}
        title={enabling ? t('anonymous.global.enableTitle') : t('anonymous.global.disableTitle')}
        description={
          enabling ? t('anonymous.global.enableBody') : t('anonymous.global.disableBody')
        }
        confirmLabel={
          enabling ? t('anonymous.global.enableConfirm') : t('anonymous.global.disableConfirm')
        }
        variant={enabling ? 'destructive' : 'default'}
        onConfirm={() => {
          if (pending !== null) update.mutate(pending)
        }}
      />
      <ConflictDialog {...update.conflict} />
    </Card>
  )
}
