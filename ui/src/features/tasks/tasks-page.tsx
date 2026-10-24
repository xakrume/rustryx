import { Link } from '@tanstack/react-router'
import { ListTodo } from 'lucide-react'
import { useCallback, useMemo } from 'react'
import { useInfinitePage } from '@/api/pagination'
import {
  DataTable,
  FilterBar,
  useTablePrefs,
  type AnyColumnDef,
  type FilterDef,
} from '@/components/data-table'
import { EmptyState } from '@/components/empty-state'
import { PageHeader } from '@/components/page-header'
import { RelativeTime } from '@/components/relative-time'
import { Alert } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import type { RunSource } from '@/features/lifecycle/api'
import { DryRunReport } from '@/features/lifecycle/dry-run-report'
import { GcReport } from '@/features/lifecycle/gc-report'
import { RunHistory } from '@/features/lifecycle/run-history'
import { RunStatusBadge } from '@/features/system/run-status'
import { useSearchParam, useSearchPatch } from '@/features/system/search-params'
import { useUrlTableState } from '@/hooks/use-url-table-state'
import { t } from '@/i18n'
import { fetchTaskPage, taskKeys, type Task } from './api'
import { describeSchedule } from './schedule'
import { durationText, TaskDetailView } from './task-detail'

const FILTER_IDS = ['kind', 'scope', 'enabled'] as const

const FILTERS: readonly FilterDef[] = [
  {
    id: 'kind',
    type: 'search',
    label: t('tasks.filters.kind'),
    placeholder: t('tasks.filters.kindPlaceholder'),
  },
  {
    id: 'scope',
    type: 'select',
    label: t('tasks.filters.scope'),
    options: [
      { value: 'system', label: t('tasks.filters.scopeSystem') },
      { value: 'user', label: t('tasks.filters.scopeUser') },
    ],
  },
  {
    id: 'enabled',
    type: 'select',
    label: t('tasks.filters.enabled'),
    options: [
      { value: 'on', label: t('tasks.filters.enabledOn') },
      { value: 'off', label: t('tasks.filters.enabledOff') },
    ],
  },
]

function columns(): AnyColumnDef<Task>[] {
  return [
    {
      accessorKey: 'name',
      header: t('tasks.columns.name'),
      meta: { label: t('tasks.columns.name'), hideable: false },
      cell: ({ row }) => (
        <div className="flex flex-col gap-0.5 py-1">
          <span className="flex items-center gap-2">
            <Link
              to="/admin/tasks"
              search={{ task: row.original.id }}
              aria-label={t('tasks.detail.open', { name: row.original.name })}
              className="text-link underline-offset-2 hover:underline"
            >
              {row.original.name}
            </Link>
            {row.original.system ? <Badge>{t('tasks.system')}</Badge> : null}
          </span>
          <span className="font-mono text-xs text-muted-foreground">
            {row.original.kind}
            {row.original.lock?.held_by ? (
              <>
                {' - '}
                <span>{t('tasks.heldBy', { node: row.original.lock.held_by })}</span>
              </>
            ) : null}
          </span>
        </div>
      ),
    },
    {
      id: 'schedule',
      header: t('tasks.columns.schedule'),
      meta: { label: t('tasks.columns.schedule') },
      cell: ({ row }) => describeSchedule(row.original.schedule),
    },
    {
      id: 'state',
      header: t('tasks.columns.state'),
      meta: { label: t('tasks.columns.state') },
      cell: ({ row }) => (
        <Badge variant={row.original.enabled ? 'success' : 'neutral'}>
          {row.original.enabled ? t('status.enabled') : t('status.disabled')}
        </Badge>
      ),
    },
    {
      id: 'lastResult',
      header: t('tasks.columns.lastResult'),
      meta: { label: t('tasks.columns.lastResult') },
      cell: ({ row }) => {
        const run = row.original.last_run
        if (run === null)
          return <span className="text-muted-foreground">{t('tasks.neverRun')}</span>
        return (
          <div className="flex flex-col items-start gap-1 py-1">
            <Link
              to="/admin/tasks/$id/runs/$run"
              params={{ id: row.original.id, run: run.id }}
              className="inline-flex"
            >
              <RunStatusBadge status={run.status} />
            </Link>
            <span className="text-xs text-muted-foreground">
              <RelativeTime value={run.finished_at ?? run.started_at ?? run.queued_at} />
              {run.error_code ? ` - ${run.error_code}` : ''}
            </span>
          </div>
        )
      },
    },
    {
      id: 'duration',
      header: t('tasks.columns.duration'),
      meta: { label: t('tasks.columns.duration'), align: 'right' },
      cell: ({ row }) => durationText(row.original.last_run?.duration_ms ?? null),
    },
    {
      id: 'nextRun',
      header: t('tasks.columns.nextRun'),
      meta: { label: t('tasks.columns.nextRun') },
      cell: ({ row }) =>
        row.original.next_run_at ? (
          <RelativeTime value={row.original.next_run_at} />
        ) : (
          <span className="text-muted-foreground">{t('tasks.noNextRun')}</span>
        ),
    },
  ]
}

function TaskList() {
  const url = useUrlTableState(FILTER_IDS)
  const { pageSize, setPageSize } = useTablePrefs('tasks')
  const { kind, scope, enabled } = url.filters
  const cols = useMemo(columns, [])
  const list = useInfinitePage<Task>({
    queryKey: [...taskKeys.list({ kind, scope, enabled }), pageSize],
    fetchPage: ({ after, signal }) =>
      fetchTaskPage({ kind, scope, enabled }, after, pageSize, signal),
  })
  return (
    <DataTable<Task>
      tableId="tasks"
      label={t('tasks.tableLabel')}
      columns={cols}
      rows={list.items}
      getRowId={(row) => row.id}
      isLoading={list.isLoading}
      error={list.isError ? list.error : undefined}
      onRetry={list.refetch}
      hasNextPage={list.hasNextPage}
      isLoadingMore={list.isLoadingMore}
      onLoadMore={list.loadMore}
      hasActiveFilters={url.hasActiveFilters}
      onClearFilters={url.clearFilters}
      pageSize={pageSize}
      onPageSizeChange={setPageSize}
      toolbar={
        <FilterBar
          filters={FILTERS}
          values={url.filters}
          onChange={url.setFilter}
          onClear={url.clearFilters}
        />
      }
      emptyState={
        <EmptyState
          icon={ListTodo}
          title={t('tasks.empty.title')}
          description={t('tasks.empty.description')}
        />
      }
    />
  )
}

function LifecycleTab() {
  const patchSearch = useSearchPatch()
  const source: RunSource = useSearchParam('source') === 'gc' ? 'gc' : 'cleanup'
  const run = useSearchParam('run')
  const mode = useSearchParam('mode')
  const status = useSearchParam('status')

  const open = useCallback((id: string) => patchSearch({ run: id }), [patchSearch])
  const changeSource = useCallback(
    (next: RunSource) =>
      patchSearch(
        { source: next === 'gc' ? 'gc' : undefined, status: undefined },
        { replace: true },
      ),
    [patchSearch],
  )
  const changeFilter = useCallback(
    (id: string, value: string | null) =>
      patchSearch({ [id]: value ?? undefined }, { replace: true }),
    [patchSearch],
  )
  const clear = useCallback(
    () => patchSearch({ mode: undefined, status: undefined }, { replace: true }),
    [patchSearch],
  )

  if (run) {
    return (
      <div className="space-y-3">
        <Button variant="outline" size="sm" onClick={() => patchSearch({ run: undefined })}>
          {t('lifecycle.backToRuns')}
        </Button>
        {source === 'gc' ? <GcReport runId={run} /> : <DryRunReport runId={run} />}
      </div>
    )
  }
  return (
    <RunHistory
      source={source}
      onSourceChange={changeSource}
      onOpen={open}
      filters={{ mode, status }}
      onFilterChange={changeFilter}
      onClearFilters={clear}
    />
  )
}

/**
 * `/admin/tasks`: the task list and, in a second tab, the cleanup and garbage collection run
 * history with its reports. A task opens as a detail view (`?task=<id>`); a run as a page of its
 * own (`/admin/tasks/:id/runs/:run`). Everything is read only in this version.
 */
export function TasksPage() {
  const task = useSearchParam('task')
  const patchSearch = useSearchPatch()
  const view = useSearchParam('view') === 'lifecycle' ? 'lifecycle' : 'tasks'
  if (task) return <TaskDetailView taskId={task} />
  return (
    <>
      <PageHeader title={t('tasks.title')} description={t('tasks.description')} />
      <Alert variant="info" className="mb-4">
        {t('tasks.readOnlyNote')}
      </Alert>
      <Tabs
        value={view}
        onValueChange={(next) =>
          patchSearch({ view: next === 'lifecycle' ? 'lifecycle' : undefined, run: undefined })
        }
      >
        <TabsList aria-label={t('tasks.tabs.label')}>
          <TabsTrigger value="tasks">{t('tasks.tabs.tasks')}</TabsTrigger>
          <TabsTrigger value="lifecycle">{t('tasks.tabs.lifecycle')}</TabsTrigger>
        </TabsList>
        <TabsContent value="tasks">
          <TaskList />
        </TabsContent>
        <TabsContent value="lifecycle">
          <LifecycleTab />
        </TabsContent>
      </Tabs>
    </>
  )
}
