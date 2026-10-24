import { useInfiniteQuery } from '@tanstack/react-query'
import { Loader2 } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
import { ErrorPanel } from '@/components/error-panel'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Label } from '@/components/ui/label'
import { Select } from '@/components/ui/select'
import { Skeleton } from '@/components/ui/skeleton'
import { t } from '@/i18n'
import { formatDateTime, parseDate } from '@/lib/format'
import { cn } from '@/lib/utils'
import { taskLogQuery, type TaskLogPage, type TaskRun } from './api'

type LogLine = TaskLogPage['lines'][number]

const LEVEL_RANK: Record<string, number> = { debug: 0, info: 1, warn: 2, error: 3 }
const MIN_RANK: Record<string, number> = { all: 0, warn: 2, error: 3 }

const LEVEL_CLASS: Record<string, string> = {
  error: 'font-semibold text-destructive-text',
  warn: 'font-semibold text-warning-text',
  info: 'text-muted-foreground',
  debug: 'text-muted-foreground',
}

const timeOf = (iso: string): string => {
  const date = parseDate(iso)
  return date === null
    ? iso
    : new Intl.DateTimeFormat(undefined, { timeStyle: 'medium' }).format(date)
}

/** While the run writes, the next window is fetched this often (the contract suggests a few seconds). */
const FOLLOW_MS = import.meta.env.MODE === 'test' ? 15 : 3_000

const LEVEL_KEYS = {
  debug: 'tasks.log.level_debug',
  info: 'tasks.log.level_info',
  warn: 'tasks.log.level_warn',
  error: 'tasks.log.level_error',
} as const

const levelLabel = (level: string): string =>
  level in LEVEL_KEYS ? t(LEVEL_KEYS[level as keyof typeof LEVEL_KEYS]) : level.toUpperCase()

/**
 * The log of a run, read forward in windows by line number. While the run is active and
 * "Follow" is on, new lines are fetched every few seconds and the view stays at the end. Level
 * names are words (ERROR, WARN), color is only a second cue. The region is focusable so
 * keyboard users can scroll it, and it is not a live region: announcing every line of a busy
 * run would drown a screen reader.
 */
export function LogViewer({ taskId, run }: { taskId: string; run: TaskRun }) {
  const [follow, setFollow] = useState(true)
  const [level, setLevel] = useState('all')
  const box = useRef<HTMLDivElement>(null)
  const query = useInfiniteQuery({ ...taskLogQuery(taskId, run.id), enabled: run.log_available })
  const { data, fetchNextPage, isFetching, isFetchingNextPage, hasNextPage } = query

  const pages = data?.pages
  const last = pages?.at(-1)
  const lines = useMemo<LogLine[]>(() => pages?.flatMap((p) => p.lines) ?? [], [pages])
  const shown = useMemo(
    () => lines.filter((line) => (LEVEL_RANK[line.level] ?? 1) >= (MIN_RANK[level] ?? 0)),
    [lines, level],
  )
  const writing = last !== undefined && !last.finished
  const purged = pages?.[0]?.purged === true

  // Follow mode: poll the next window while the run is still writing.
  useEffect(() => {
    if (!follow || !writing || isFetching) return undefined
    const id = setTimeout(() => void fetchNextPage(), FOLLOW_MS)
    return () => clearTimeout(id)
  }, [follow, writing, isFetching, fetchNextPage, lines.length])

  // Stay at the end while following.
  useEffect(() => {
    const element = box.current
    if (follow && writing && element) element.scrollTop = element.scrollHeight
  }, [follow, writing, shown.length])

  if (!run.log_available) {
    // A run whose retention date has passed lost its log; one that never had a log says so.
    const expires = parseDate(run.log_expires_at)
    return expires !== null && expires.getTime() < Date.now() ? (
      <Alert variant="warning">{t('tasks.log.purged', { date: formatDateTime(expires) })}</Alert>
    ) : (
      <p className="text-sm text-muted-foreground">{t('tasks.log.notAvailable')}</p>
    )
  }
  if (query.isError && !data) {
    return (
      <ErrorPanel
        error={query.error}
        onRetry={() => void query.refetch()}
        title={t('tasks.log.loadFailed')}
      />
    )
  }
  if (!data) return <Skeleton className="h-48" aria-busy="true" />
  if (purged) {
    const expires = parseDate(run.log_expires_at)
    return (
      <Alert variant="warning">
        {expires
          ? t('tasks.log.purged', { date: formatDateTime(expires) })
          : t('tasks.log.purgedUnknown')}
      </Alert>
    )
  }

  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <label className="flex flex-col gap-1 text-xs font-medium">
          <span>{t('tasks.log.level')}</span>
          <Select value={level} className="min-w-40" onChange={(e) => setLevel(e.target.value)}>
            <option value="all">{t('tasks.log.levelAll')}</option>
            <option value="warn">{t('tasks.log.level_warn')}</option>
            <option value="error">{t('tasks.log.level_error')}</option>
          </Select>
        </label>
        {writing ? (
          <div className="flex items-center gap-2">
            <Checkbox
              id={`follow-${run.id}`}
              checked={follow}
              onCheckedChange={(checked) => setFollow(checked === true)}
            />
            <Label htmlFor={`follow-${run.id}`}>{t('tasks.log.follow')}</Label>
          </div>
        ) : null}
      </div>
      <div
        ref={box}
        role="log"
        aria-live="off"
        aria-label={t('tasks.log.label')}
        // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- keyboard users must be able to scroll the log
        tabIndex={0}
        className="max-h-96 overflow-auto rounded-lg border border-border bg-muted p-2 font-mono text-[0.8125rem] leading-5"
      >
        {shown.length === 0 ? (
          <p className="p-2 text-muted-foreground">{t('tasks.log.empty')}</p>
        ) : (
          shown.map((line) => (
            <div key={line.n} className="flex gap-3 whitespace-pre-wrap">
              <span className="w-16 shrink-0 text-right text-muted-foreground tabular-nums">
                {line.n}
              </span>
              <time dateTime={line.at} className="shrink-0 text-muted-foreground">
                {timeOf(line.at)}
              </time>
              <span className={cn('w-12 shrink-0', LEVEL_CLASS[line.level])}>
                {levelLabel(line.level)}
              </span>
              <span className="min-w-0 break-words">{line.message}</span>
            </div>
          ))
        )}
      </div>
      <div className="flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground">
        <p role="status">
          {t('tasks.log.lines', { count: lines.length })}
          {writing ? ` - ${t('tasks.log.following')}` : ` - ${t('tasks.log.finished')}`}
        </p>
        {writing && follow ? (
          <Loader2 className="size-4 animate-spin-slow" aria-hidden="true" />
        ) : null}
        {!writing && hasNextPage ? (
          <Button
            variant="outline"
            size="sm"
            loading={isFetchingNextPage}
            onClick={() => void fetchNextPage()}
          >
            {isFetchingNextPage ? t('tasks.log.loadingMore') : t('tasks.log.loadMore')}
          </Button>
        ) : null}
      </div>
      {query.isError ? (
        <ErrorPanel error={query.error} onRetry={() => void fetchNextPage()} />
      ) : null}
    </div>
  )
}
