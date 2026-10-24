import { RelativeTime } from '@/components/relative-time'
import { StatusBadge, type StatusKind } from '@/components/status-badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { t } from '@/i18n'
import type { StatusCheck } from './api'
import { checkById, DEPENDENCIES } from './status-model'

const KIND: Record<string, StatusKind> = {
  ok: 'ok',
  warning: 'warning',
  error: 'error',
  unknown: 'unknown',
}

const LABEL = {
  ok: 'systemStatus.dependencies.reachable',
  warning: 'systemStatus.dependencies.degraded',
  error: 'systemStatus.dependencies.unreachable',
  unknown: 'systemStatus.dependencies.unknown',
} as const

/**
 * Health of what the node talks to. The words are about the dependency ("Reachable",
 * "Unreachable"), never about the node, and the card says out loud that none of it changes the
 * readiness shown next to it. A dependency the server does not report is "not reported", not green.
 */
export function DependenciesCard({ checks }: { checks: readonly StatusCheck[] }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('systemStatus.dependencies.title')}</CardTitle>
        <p className="text-sm text-muted-foreground">{t('systemStatus.dependencies.note')}</p>
      </CardHeader>
      <CardContent>
        <ul
          className="divide-y divide-border text-sm"
          aria-label={t('systemStatus.dependencies.tableLabel')}
        >
          {DEPENDENCIES.map((dependency) => {
            const check = checkById(checks, dependency.id)
            const status = check?.status ?? 'unknown'
            const known = status in LABEL
            return (
              <li key={dependency.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 py-2">
                <span className="w-28 font-medium">{t(dependency.label)}</span>
                {check ? (
                  <StatusBadge
                    status={KIND[status] ?? 'unknown'}
                    label={t(known ? LABEL[status] : LABEL.unknown)}
                  />
                ) : (
                  <StatusBadge status="neutral" label={t('systemStatus.dependencies.unknown')} />
                )}
                <span className="min-w-0 flex-1 text-muted-foreground">
                  {check ? check.message : t('systemStatus.dependencies.notReported')}
                </span>
                {check?.since ? (
                  <span className="text-xs text-muted-foreground">
                    {t('systemStatus.dependencies.since', { time: '' })}
                    <RelativeTime value={check.since} />
                  </span>
                ) : null}
              </li>
            )
          })}
        </ul>
      </CardContent>
    </Card>
  )
}
