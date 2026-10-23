import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { api } from '@/api/client'
import { serviceStatus, useServiceState } from '@/api/service-status'
import { unwrap } from '@/api/request'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { useMe, usePermissions } from '@/features/auth/use-permissions'
import { useCountdown } from '@/hooks/use-countdown'
import { useOnlineStatus } from '@/hooks/use-online-status'
import { t } from '@/i18n'

const stripClass = 'rounded-none border-x-0 border-t-0'

/**
 * Persistent banners above the page content (docs/ui-page-inventory.md, section 5):
 * offline, server unreachable, server asked to wait (Retry-After countdown), read-only mode,
 * and for administrators a degraded system. Stale data stays visible under them.
 */
export function Banners() {
  const queryClient = useQueryClient()
  const online = useOnlineStatus()
  const service = useServiceState()
  const me = useMe()
  const permissions = usePermissions()
  const secondsLeft = useCountdown(service.retryUntil)

  // Administrators only: the check list needs nx-admin, so nobody else even asks.
  const status = useQuery({
    queryKey: ['system', 'status', 'banner'],
    queryFn: ({ signal }) => unwrap(api.GET('/api/v1/system/status', { signal })),
    enabled: permissions.hasArea('system'),
    staleTime: 60_000,
    refetchInterval: 120_000,
    meta: { silent: true },
    retry: false,
  })

  function retryNow(): void {
    serviceStatus.clearRetry()
    void queryClient.refetchQueries({ type: 'active' })
  }

  return (
    <div className="flex flex-col">
      {!online ? (
        <Alert variant="warning" className={stripClass}>
          {t('shell.banners.offline')}
        </Alert>
      ) : service.backendDown ? (
        <Alert variant="destructive" className={stripClass}>
          <span>{t('shell.banners.backendDown')}</span>{' '}
          <Button variant="link" size="sm" className="h-auto p-0" onClick={retryNow}>
            {t('shell.banners.backendDownRetry')}
          </Button>
        </Alert>
      ) : null}
      {service.retryUntil !== null && secondsLeft > 0 ? (
        <Alert variant="warning" className={stripClass}>
          <span>{t('shell.banners.retryPaused', { seconds: secondsLeft })}</span>{' '}
          <Button variant="link" size="sm" className="h-auto p-0" onClick={retryNow}>
            {t('shell.banners.retryNow')}
          </Button>
        </Alert>
      ) : null}
      {me?.system.read_only ? (
        <Alert variant="warning" className={stripClass}>
          {t('shell.banners.readOnly')}
        </Alert>
      ) : null}
      {status.data && status.data.status !== 'ok' ? (
        <Alert variant="warning" className={stripClass}>
          <span>{t('shell.banners.degraded')}</span>{' '}
          <Link to="/admin/system/status" className="font-medium underline underline-offset-2">
            {t('shell.banners.degradedLink')}
          </Link>
        </Alert>
      ) : null}
    </div>
  )
}
