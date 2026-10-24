import { Link } from '@tanstack/react-router'
import { RelativeTime } from '@/components/relative-time'
import { StatusBadge, type StatusKind } from '@/components/status-badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { t } from '@/i18n'
import type { StatusCheck } from './api'
import { attentionChecks } from './status-model'

const KIND: Record<string, StatusKind> = {
  ok: 'ok',
  warning: 'warning',
  error: 'error',
  unknown: 'unknown',
}

const SEVERITY = {
  info: 'systemStatus.checks.severity.info',
  warning: 'systemStatus.checks.severity.warning',
  critical: 'systemStatus.checks.severity.critical',
} as const

function CheckList({ checks }: { checks: readonly StatusCheck[] }) {
  return (
    <ul className="divide-y divide-border text-sm">
      {checks.map((check) => (
        <li key={check.id} className="space-y-1 py-2">
          <div className="flex flex-wrap items-center gap-2">
            {/* `unknown` is never rendered as green: the check could not run. */}
            <StatusBadge status={KIND[check.status] ?? 'unknown'} />
            <span className="font-medium">{check.title}</span>
            <span className="text-xs text-muted-foreground">
              {t(SEVERITY[check.severity] ?? SEVERITY.info)}
            </span>
            {check.since ? (
              <span className="text-xs text-muted-foreground">
                {t('systemStatus.checks.since', { time: '' })}
                <RelativeTime value={check.since} />
              </span>
            ) : null}
          </div>
          <p className="text-muted-foreground">{check.message}</p>
          {check.fix_hint && check.status !== 'ok' ? (
            <p>{t('systemStatus.checks.fix', { hint: check.fix_hint })}</p>
          ) : null}
          {check.link && check.link.startsWith('/') ? (
            <Link to={check.link} className="inline-block text-link underline underline-offset-2">
              {t('systemStatus.checks.open', { title: check.title })}
            </Link>
          ) : null}
        </li>
      ))}
    </ul>
  )
}

/**
 * Everything that is not a dependency probe, worst first, each with its fix hint and a link to
 * the page that deals with it. When every check passes the list collapses into one line, so a
 * healthy system is quiet.
 */
export function ChecksCard({ checks }: { checks: readonly StatusCheck[] }) {
  const list = attentionChecks(checks)
  const allGreen = list.length > 0 && list.every((c) => c.status === 'ok')
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('systemStatus.checks.title')}</CardTitle>
      </CardHeader>
      <CardContent>
        {list.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t('systemStatus.checks.empty')}</p>
        ) : allGreen ? (
          <details>
            <summary className="cursor-pointer text-sm">
              {t('systemStatus.checks.allGreen', { count: list.length })}{' '}
              <span className="text-link underline underline-offset-2">
                {t('systemStatus.checks.showAll')}
              </span>
            </summary>
            <CheckList checks={list} />
          </details>
        ) : (
          <CheckList checks={list} />
        )}
      </CardContent>
    </Card>
  )
}
