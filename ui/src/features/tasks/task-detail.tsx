import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { useMemo, useState } from 'react'
import { useInfinitePage } from '@/api/pagination'
import {
  DataTable,
  FilterBar,
  useTablePrefs,
  type AnyColumnDef,
  type FilterDef,
} from '@/components/data-table'
import { ErrorPanel } from '@/components/error-panel'
import { PageHeader } from '@/components/page-header'
import { RelativeTime } from '@/components/relative-time'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { RunStatusBadge } from '@/features/system/run-status'
import { useSearchPatch } from '@/features/system/search-params'
import { t } from '@/i18n'
import { formatDateTime, formatDuration, parseDate } from '@/lib/format'
import { fetchTaskRunPage, taskKeys, taskQuery, type Task, type TaskRun } from './api'
import { describeSchedule } from './schedule'
import { summaryValue } from './summary-format'

const STATUS_FILTER: readonly FilterDef[] = [
  {
    id: 'status',
    type: 'select',
    label: t('tasks.detail.runFilters'),
    options: ['queued', 'running', 'succeeded', 'failed', 'cancelled', 'aborted'].map((value) => ({
      value,
      label: t(`runStatus.${value as 'queued'}`),
    })),
  },
]

export const summaryText = (summary: Record<string, unknown>): string =>
  Object.entries(summary)
    .map(([key, value]) => `${key}: ${summaryValue(key, value)}`)
    .join(', ')

export const durationText = (ms: number | null): string =>
  ms === null ? '-' : formatDuration(ms / 1000)

function runColumns(task: Task): AnyColumnDef<TaskRun>[] {
  return [
    {
      id: 'started',
      header: t('tasks.runs.columns.started'),
      meta: { label: t('tasks.runs.columns.started'), hideable: false },
      cell: ({ row }) => {
        const when = row.original.started_at ?? row.original.queued_at
        const date = parseDate(when)
        return (
          <Link
            to="/admin/tasks/$id/runs/$run"
            params={{ id: task.id, run: row.original.id }}
            aria-label={t('tasks.runs.openRun', { time: date ? formatDateTime(date) : when })}
            className="text-link underline-offset-2 hover:underline"
          >
            <RelativeTime value={when} />
          </Link>
        )
      },
    },
    {
      id: 'status',
      header: t('tasks.runs.columns.status'),
      meta: { label: t('tasks.runs.columns.status') },
      cell: ({ row }) => (
        <div className="flex flex-col items-start gap-1 py-1">
          <span className="flex items-center gap-1.5">
            <RunStatusBadge status={row.original.status} />
            {row.original.dry_run ? <Badge>{t('tasks.runs.dryRun')}</Badge> : null}
          </span>
          {row.original.status === 'queued' && task.lock?.held_by ? (
            <span className="text-xs text-muted-foreground">
              {t('tasks.detail.queuedWaiting', { node: task.lock.held_by })}
            </span>
          ) : null}
        </div>
      ),
    },
    {
      id: 'trigger',
      header: t('tasks.runs.columns.trigger'),
      meta: { label: t('tasks.runs.columns.trigger') },
      cell: ({ row }) => {
        const trigger = row.original.trigger
        return t(`tasks.runs.trigger.${trigger}`)
      },
    },
    {
      id: 'node',
      header: t('tasks.runs.columns.node'),
      meta: { label: t('tasks.runs.columns.node') },
      cell: ({ row }) => <span className="font-mono">{row.original.node ?? '-'}</span>,
    },
    {
      id: 'duration',
      header: t('tasks.runs.columns.duration'),
      meta: { label: t('tasks.runs.columns.duration'), align: 'right' },
      cell: ({ row }) => durationText(row.original.duration_ms),
    },
    {
      id: 'result',
      header: t('tasks.runs.columns.result'),
      meta: { label: t('tasks.runs.columns.result'), className: 'min-w-56 whitespace-normal' },
      cell: ({ row }) =>
        row.original.error_code ? (
          <span className="font-mono text-destructive-text">
            {t('tasks.runs.error', { code: row.original.error_code })}
          </span>
        ) : Object.keys(row.original.summary).length > 0 ? (
          <span className="font-mono text-xs">{summaryText(row.original.summary)}</span>
        ) : (
          <span className="text-muted-foreground">{t('tasks.runs.noSummary')}</span>
        ),
    },
  ]
}

function Overview({ task }: { task: Task }) {
  const nextRuns = task.schedule?.next_runs ?? []
  const params = Object.entries(task.parameters)
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('tasks.detail.overview')}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-4 text-sm">
        {task.description ? <p>{task.description}</p> : null}
        <dl className="grid gap-x-6 gap-y-2 sm:grid-cols-2">
          <div>
            <dt className="text-muted-foreground">{t('tasks.columns.kind')}</dt>
            <dd className="font-mono">{task.kind}</dd>
          </div>
          <div>
            <dt className="text-muted-foreground">{t('tasks.columns.state')}</dt>
            <dd className="flex flex-wrap items-center gap-2">
              <Badge variant={task.enabled ? 'success' : 'neutral'}>
                {task.enabled ? t('status.enabled') : t('status.disabled')}
              </Badge>
              <Badge>{task.system ? t('tasks.system') : t('tasks.user')}</Badge>
            </dd>
          </div>
          <div>
            <dt className="text-muted-foreground">{t('tasks.columns.schedule')}</dt>
            <dd>{describeSchedule(task.schedule)}</dd>
          </div>
          <div>
            <dt className="text-muted-foreground">{t('tasks.columns.nextRun')}</dt>
            <dd>
              {task.next_run_at ? <RelativeTime value={task.next_run_at} /> : t('tasks.noNextRun')}
            </dd>
          </div>
        </dl>
        <div>
          <h3 className="mb-1 font-medium">{t('tasks.detail.nextRuns')}</h3>
          {nextRuns.length === 0 ? (
            <p className="text-muted-foreground">{t('tasks.detail.noNextRuns')}</p>
          ) : (
            <ol className="list-decimal space-y-0.5 pl-5">
              {nextRuns.map((iso) => {
                const date = parseDate(iso)
                return <li key={iso}>{date ? formatDateTime(date) : iso}</li>
              })}
            </ol>
          )}
        </div>
        <div>
          <h3 className="mb-1 font-medium">{t('tasks.detail.parameters')}</h3>
          {params.length === 0 ? (
            <p className="text-muted-foreground">{t('tasks.detail.noParameters')}</p>
          ) : (
            <dl className="grid gap-x-6 gap-y-1 sm:grid-cols-2">
              {params.map(([key, value]) => (
                <div key={key}>
                  <dt className="font-mono text-xs text-muted-foreground">{key}</dt>
                  <dd className="font-mono">{String(value)}</dd>
                </div>
              ))}
            </dl>
          )}
        </div>
        {task.lock !== null ? (
          <p className="rounded-md border border-border bg-muted px-3 py-2">
            <span className="font-medium">{t('tasks.detail.lockTitle')}: </span>
            {task.lock.held_by
              ? t('tasks.detail.lockHeld', { node: task.lock.held_by })
              : t('tasks.detail.lockFree')}
          </p>
        ) : null}
      </CardContent>
    </Card>
  )
}

function RunHistory({ task }: { task: Task }) {
  const [status, setStatus] = useState<string | undefined>(undefined)
  const { pageSize, setPageSize } = useTablePrefs('task-runs')
  const columns = useMemo(() => runColumns(task), [task])
  const list = useInfinitePage<TaskRun>({
    queryKey: [...taskKeys.runs(task.id, status), pageSize],
    fetchPage: ({ after, signal }) => fetchTaskRunPage(task.id, status, after, pageSize, signal),
  })
  return (
    <DataTable<TaskRun>
      tableId="task-runs"
      label={t('tasks.detail.historyLabel', { name: task.name })}
      columns={columns}
      rows={list.items}
      getRowId={(row) => row.id}
      isLoading={list.isLoading}
      error={list.isError ? list.error : undefined}
      onRetry={list.refetch}
      hasNextPage={list.hasNextPage}
      isLoadingMore={list.isLoadingMore}
      onLoadMore={list.loadMore}
      hasActiveFilters={status !== undefined}
      onClearFilters={() => setStatus(undefined)}
      pageSize={pageSize}
      onPageSizeChange={setPageSize}
      toolbar={
        <FilterBar
          filters={STATUS_FILTER}
          values={{ status }}
          onChange={(_id, value) => setStatus(value ?? undefined)}
          onClear={() => setStatus(undefined)}
        />
      }
      emptyState={
        <p className="rounded-lg border border-dashed border-input p-8 text-center text-sm text-muted-foreground">
          {t('tasks.detail.historyEmpty')}
        </p>
      }
    />
  )
}

/** `/admin/tasks?task=<id>`: what the task is, when it runs next, and every run it had. */
export function TaskDetailView({ taskId }: { taskId: string }) {
  const patchSearch = useSearchPatch()
  const query = useQuery(taskQuery(taskId))
  const task = query.data
  return (
    <>
      <PageHeader
        title={task?.name ?? t('tasks.title')}
        description={t('tasks.readOnlyNote')}
        actions={
          <Button variant="outline" onClick={() => patchSearch({ task: undefined })}>
            {t('tasks.detail.back')}
          </Button>
        }
      />
      {query.isError && !task ? (
        <ErrorPanel error={query.error} onRetry={() => void query.refetch()} />
      ) : !task ? (
        <Skeleton className="h-64" aria-busy="true" />
      ) : (
        <div className="space-y-4">
          <Overview task={task} />
          <section aria-labelledby="task-history" className="space-y-2">
            <h2 id="task-history" className="text-base font-semibold">
              {t('tasks.detail.history')}
            </h2>
            <RunHistory task={task} />
          </section>
        </div>
      )}
    </>
  )
}
