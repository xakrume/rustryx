import { Link } from '@tanstack/react-router'
import { Check, Circle } from 'lucide-react'
import type { ReactNode } from 'react'
import { BytesText } from '@/components/bytes-text'
import { RelativeTime } from '@/components/relative-time'
import { StatusBadge, type StatusKind } from '@/components/status-badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Meter, quotaTone } from '@/features/components/meter'
import { formatLabel } from '@/features/repositories/repo-meta'
import { t } from '@/i18n'
import type { DashboardSummary } from './api'

const HEALTH_STATUS: Record<string, StatusKind> = {
  ok: 'ok',
  degraded: 'warning',
  unreachable: 'error',
  unknown: 'unknown',
}

function CountRow({
  to,
  search,
  label,
  count,
}: {
  to: string
  search: Record<string, string>
  label: ReactNode
  count: number
}) {
  return (
    <li>
      <Link
        to={to}
        search={search}
        className="flex justify-between gap-4 rounded-sm py-0.5 hover:bg-muted"
      >
        <span className="font-mono text-link underline-offset-2 hover:underline">{label}</span>
        <span className="tabular">{count.toLocaleString()}</span>
      </Link>
    </li>
  )
}

/** Counts by format and type; each row opens the repository list already filtered to it. */
export function RepositoriesCard({ summary }: { summary: DashboardSummary['repositories'] }) {
  const byFormat = Object.entries(summary.by_format).sort(([, a], [, b]) => b - a)
  const byType = Object.entries(summary.by_type).sort(([, a], [, b]) => b - a)
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('dashboard.repositories')}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        <p className="text-2xl font-semibold tabular">
          <Link to="/repositories" className="underline-offset-2 hover:underline">
            {t('dashboard.repositoriesTotal', { count: summary.total })}
          </Link>
        </p>
        {summary.offline > 0 ? (
          <Link to="/repositories" search={{ online: 'false' }} className="inline-block">
            <StatusBadge status="warning" label={`${t('dashboard.offline')}: ${summary.offline}`} />
          </Link>
        ) : null}
        <div className="grid grid-cols-2 gap-4 text-sm">
          <div>
            <h3 className="mb-1 text-xs font-medium text-muted-foreground">
              {t('dashboard.byFormat')}
            </h3>
            <ul className="m-0 list-none space-y-0.5 p-0">
              {byFormat.map(([format, count]) => (
                <CountRow
                  key={format}
                  to="/repositories"
                  search={{ format }}
                  label={formatLabel(format)}
                  count={count}
                />
              ))}
            </ul>
          </div>
          <div>
            <h3 className="mb-1 text-xs font-medium text-muted-foreground">
              {t('dashboard.byType')}
            </h3>
            <ul className="m-0 list-none space-y-0.5 p-0">
              {byType.map(([type, count]) => (
                <CountRow
                  key={type}
                  to="/repositories"
                  search={{ repo_type: type }}
                  label={t(`repos.type.${type as 'hosted' | 'proxy' | 'group'}`)}
                  count={count}
                />
              ))}
            </ul>
          </div>
        </div>
      </CardContent>
    </Card>
  )
}

type Storage = NonNullable<DashboardSummary['storage']>

/** Usage per blob store; a store with a quota gets a meter and its percentage as text. */
export function StorageCard({ stores }: { stores: Storage }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('dashboard.storage')}</CardTitle>
      </CardHeader>
      <CardContent>
        <ul className="m-0 list-none space-y-4 p-0">
          {stores.map((store) => {
            const percent =
              store.quota_bytes !== null && store.quota_bytes > 0
                ? Math.round((store.used_bytes / store.quota_bytes) * 100)
                : null
            return (
              <li key={store.blob_store} className="space-y-1.5">
                <div className="flex items-center justify-between gap-2">
                  <span className="font-mono text-sm">{store.blob_store}</span>
                  <StatusBadge
                    status={HEALTH_STATUS[store.health] ?? 'unknown'}
                    label={t(`dashboard.health.${store.health}`)}
                  />
                </div>
                {percent !== null ? (
                  <Meter
                    percent={percent}
                    tone={quotaTone(percent)}
                    label={t('home.usageOf', { name: store.blob_store })}
                  />
                ) : null}
                <p className="text-sm text-muted-foreground">
                  <BytesText bytes={store.used_bytes} />{' '}
                  {store.quota_bytes === null ? (
                    t('home.usedNoQuota')
                  ) : (
                    <>
                      {t('dashboard.storageQuota', { quota: '' })}
                      <BytesText bytes={store.quota_bytes} />
                      {percent !== null ? ` (${percent}%)` : ''}
                    </>
                  )}
                </p>
              </li>
            )
          })}
        </ul>
      </CardContent>
    </Card>
  )
}

type Step = NonNullable<DashboardSummary['setup_checklist']>[number]

export function SetupCard({ steps }: { steps: readonly Step[] }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('dashboard.setup')}</CardTitle>
      </CardHeader>
      <CardContent>
        <ul className="m-0 list-none space-y-2 p-0 text-sm">
          {steps.map((step) => (
            <li key={step.id} className="flex items-center gap-2">
              {step.done ? (
                <Check className="size-4 text-success-text" aria-hidden="true" />
              ) : (
                <Circle className="size-4 text-muted-foreground" aria-hidden="true" />
              )}
              {/* The status is text as well as an icon: "Done" or "To do" for screen readers. */}
              <span className="sr-only">
                {step.done ? t('dashboard.done') : t('dashboard.todo')}:{' '}
              </span>
              {step.done ? (
                <span>{t(`dashboard.checklist.${step.id}`)}</span>
              ) : (
                <Link to={step.link} className="text-link underline underline-offset-2">
                  {t(`dashboard.checklist.${step.id}`)}
                </Link>
              )}
            </li>
          ))}
        </ul>
      </CardContent>
    </Card>
  )
}

type Run = NonNullable<DashboardSummary['failed_tasks']>[number]

/** Task runs that failed: only shown when there are some, with the code and a link to the tasks. */
export function FailedTasksCard({ runs }: { runs: readonly Run[] }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('home.failedTasks')}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-2">
        <ul className="m-0 list-none divide-y divide-border p-0 text-sm">
          {runs.map((run) => (
            <li key={run.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 py-1.5">
              <StatusBadge status="error" label={t('home.failed')} />
              {run.error_code ? <code className="font-mono">{run.error_code}</code> : null}
              <RelativeTime
                value={run.finished_at ?? run.queued_at}
                className="ml-auto text-muted-foreground"
              />
            </li>
          ))}
        </ul>
        <Link to="/admin/tasks" className="text-sm text-link underline underline-offset-2">
          {t('home.openTasks')}
        </Link>
      </CardContent>
    </Card>
  )
}

type Event = NonNullable<DashboardSummary['recent_activity']>[number]

export function ActivityCard({ events, showAll }: { events: readonly Event[]; showAll: boolean }) {
  return (
    <Card className="md:col-span-2 xl:col-span-3">
      <CardHeader className="flex-row items-center justify-between">
        <CardTitle>{t('dashboard.recentActivity')}</CardTitle>
        {showAll ? (
          <Link to="/admin/audit" className="text-sm text-link underline-offset-2 hover:underline">
            {t('home.viewAudit')}
          </Link>
        ) : null}
      </CardHeader>
      <CardContent>
        {events.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t('dashboard.noActivity')}</p>
        ) : (
          <ul className="m-0 list-none divide-y divide-border p-0 text-sm">
            {events.map((event) => (
              <li key={event.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 py-1.5">
                <RelativeTime value={event.at} className="w-28 text-muted-foreground" />
                <span className="font-mono">{event.action}</span>
                <span className="text-muted-foreground">{event.actor_name}</span>
                {event.target ? <span className="font-mono">{event.target}</span> : null}
                <StatusBadge
                  status={event.outcome === 'success' ? 'ok' : 'error'}
                  label={event.outcome}
                  className="ml-auto"
                />
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  )
}
