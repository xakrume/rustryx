import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { Loader2 } from 'lucide-react'
import { ErrorPanel } from '@/components/error-panel'
import { PageHeader } from '@/components/page-header'
import { RelativeTime } from '@/components/relative-time'
import { Alert } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { RunStatusBadge } from '@/features/system/run-status'
import { t } from '@/i18n'
import { taskQuery, taskRunQuery } from './api'
import { LogViewer } from './log-viewer'
import { summaryValue } from './summary-format'
import { durationText } from './task-detail'

/** `/admin/tasks/:id/runs/:run`: status, timing, node, counters, and the log of one run. */
export function TaskRunPage({ taskId, runId }: { taskId: string; runId: string }) {
  const task = useQuery(taskQuery(taskId))
  const query = useQuery(taskRunQuery(taskId, runId))
  const run = query.data
  const name = task.data?.name ?? taskId
  const active = run?.status === 'queued' || run?.status === 'running'
  const summary = run ? Object.entries(run.summary) : []

  return (
    <>
      <PageHeader
        title={t('tasks.run.title', { name })}
        description={
          run ? (
            <span className="flex flex-wrap items-center gap-2">
              <RunStatusBadge status={run.status} />
              {run.dry_run ? <Badge>{t('tasks.runs.dryRun')}</Badge> : null}
              <RelativeTime value={run.started_at ?? run.queued_at} />
            </span>
          ) : undefined
        }
        actions={
          <Button asChild variant="outline">
            <Link to="/admin/tasks" search={{ task: taskId }}>
              {t('tasks.run.back')}
            </Link>
          </Button>
        }
      />
      {query.isError && !run ? (
        <ErrorPanel
          error={query.error}
          onRetry={() => void query.refetch()}
          title={t('tasks.run.loadFailed')}
        />
      ) : !run ? (
        <Skeleton className="h-64" aria-busy="true" />
      ) : (
        <div className="space-y-4">
          {run.status === 'failed' ? (
            <Alert variant="destructive" title={t('tasks.run.errorTitle')}>
              {t('tasks.run.errorDetail', { code: run.error_code ?? 'unknown' })}
            </Alert>
          ) : null}
          {run.status === 'aborted' ? (
            <Alert variant="warning" title={t('tasks.run.abortedTitle')}>
              {t('tasks.run.abortedDetail')}
            </Alert>
          ) : null}
          {active ? (
            <Alert variant="info">
              <span className="flex items-center gap-2">
                <Loader2 className="size-4 animate-spin-slow" aria-hidden="true" />
                {t('tasks.run.inProgress')}
              </span>
            </Alert>
          ) : null}
          <Card>
            <CardContent className="pt-4">
              <dl className="grid gap-x-6 gap-y-3 text-sm sm:grid-cols-2 lg:grid-cols-4">
                <div>
                  <dt className="text-muted-foreground">{t('tasks.run.trigger')}</dt>
                  <dd>{t(`tasks.runs.trigger.${run.trigger}`)}</dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">{t('tasks.run.triggeredBy')}</dt>
                  <dd>{run.triggered_by?.username ?? '-'}</dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">{t('tasks.run.node')}</dt>
                  <dd className="font-mono">{run.node ?? '-'}</dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">{t('tasks.run.duration')}</dt>
                  <dd className="tabular-nums">{durationText(run.duration_ms)}</dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">{t('tasks.run.queued')}</dt>
                  <dd>
                    <RelativeTime value={run.queued_at} />
                  </dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">{t('tasks.run.started')}</dt>
                  <dd>
                    <RelativeTime value={run.started_at} />
                  </dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">{t('tasks.run.finished')}</dt>
                  <dd>
                    <RelativeTime value={run.finished_at} />
                  </dd>
                </div>
              </dl>
            </CardContent>
          </Card>
          <Card>
            <CardHeader>
              <CardTitle>{t('tasks.run.summary')}</CardTitle>
            </CardHeader>
            <CardContent>
              {summary.length === 0 ? (
                <p className="text-sm text-muted-foreground">{t('tasks.run.noSummary')}</p>
              ) : (
                <dl className="grid gap-x-6 gap-y-2 text-sm sm:grid-cols-2 lg:grid-cols-4">
                  {summary.map(([key, value]) => (
                    <div key={key}>
                      <dt className="font-mono text-xs text-muted-foreground">{key}</dt>
                      <dd className="font-mono">{summaryValue(key, value)}</dd>
                    </div>
                  ))}
                </dl>
              )}
            </CardContent>
          </Card>
          <Card>
            <CardHeader>
              <CardTitle>{t('tasks.log.title')}</CardTitle>
            </CardHeader>
            <CardContent>
              <LogViewer taskId={taskId} run={run} />
            </CardContent>
          </Card>
        </div>
      )}
    </>
  )
}
