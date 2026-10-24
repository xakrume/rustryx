import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { RelativeTime } from '@/components/relative-time'
import { StatusBadge } from '@/components/status-badge'
import { metricsQuery } from '@/features/system/api'
import { fetchTaskPage } from '@/features/tasks/api'
import { t } from '@/i18n'
import { WidgetCard } from './widget-card'

const TASK_SCAN_LIMIT = 100

/**
 * Health of the background work: each job's last success from the node's counters and the tasks
 * whose last run failed or was aborted, each linking to its run. Stale jobs and failed runs
 * are words and icons, never only a color.
 */
export function JobHealthWidget({ className }: { className?: string }) {
  const metrics = useQuery(metricsQuery)
  const tasks = useQuery({
    queryKey: ['system-widgets', 'tasks'],
    queryFn: ({ signal }) =>
      fetchTaskPage(
        { kind: undefined, scope: undefined, enabled: undefined },
        null,
        TASK_SCAN_LIMIT,
        signal,
      ),
    staleTime: 0,
  })
  const jobs = metrics.data?.background_jobs ?? []
  const failed = (tasks.data?.items ?? []).filter(
    (task) => task.last_run?.status === 'failed' || task.last_run?.status === 'aborted',
  )
  const error = metrics.error ?? tasks.error
  return (
    <WidgetCard
      title={t('systemWidgets.jobs.title')}
      description={t('systemWidgets.jobs.description')}
      loading={metrics.isPending || tasks.isPending}
      error={error}
      onRetry={() => {
        void metrics.refetch()
        void tasks.refetch()
      }}
      hasData={metrics.data !== undefined && tasks.data !== undefined}
      className={className}
      actions={
        <Link to="/admin/tasks" className="text-sm text-link underline underline-offset-2">
          {t('systemWidgets.jobs.viewTasks')}
        </Link>
      }
    >
      <div className="space-y-4 text-sm">
        {jobs.length === 0 ? (
          <p className="text-muted-foreground">{t('systemWidgets.jobs.noJobs')}</p>
        ) : (
          <ul aria-label={t('systemWidgets.jobs.tableLabel')} className="divide-y divide-border">
            {jobs.map((job) => (
              <li key={job.task} className="flex flex-wrap items-center gap-x-3 gap-y-1 py-1.5">
                <span className="min-w-0 flex-1 font-mono">{job.task}</span>
                <span className="text-muted-foreground">
                  {t('systemWidgets.jobs.lastSuccess')}:{' '}
                  {job.last_success_at ? (
                    <RelativeTime value={job.last_success_at} />
                  ) : (
                    t('systemWidgets.jobs.never')
                  )}
                </span>
                <StatusBadge
                  status={job.stale ? 'warning' : 'ok'}
                  label={t(job.stale ? 'systemWidgets.jobs.stale' : 'systemWidgets.jobs.onTime')}
                />
              </li>
            ))}
          </ul>
        )}
        <div>
          <h3 className="mb-1 font-medium">{t('systemWidgets.jobs.failedTitle')}</h3>
          {failed.length === 0 ? (
            <p className="text-muted-foreground">{t('systemWidgets.jobs.failedNone')}</p>
          ) : (
            <ul className="space-y-1">
              {failed.map((task) => (
                <li key={task.id} className="flex flex-wrap items-center gap-2">
                  <StatusBadge
                    status={task.last_run?.status === 'failed' ? 'error' : 'warning'}
                    label={
                      task.last_run?.status === 'failed'
                        ? t('runStatus.failed')
                        : t('runStatus.aborted')
                    }
                  />
                  {task.last_run ? (
                    <Link
                      to="/admin/tasks/$id/runs/$run"
                      params={{ id: task.id, run: task.last_run.id }}
                      className="text-link underline underline-offset-2"
                    >
                      {task.name}
                    </Link>
                  ) : (
                    task.name
                  )}
                  {task.last_run?.error_code ? (
                    <code className="font-mono text-xs text-muted-foreground">
                      {task.last_run.error_code}
                    </code>
                  ) : null}
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>
    </WidgetCard>
  )
}
