import { useQuery } from '@tanstack/react-query'
import { Download, Loader2 } from 'lucide-react'
import { useState } from 'react'
import { ErrorPanel } from '@/components/error-panel'
import { BytesText } from '@/components/bytes-text'
import { RelativeTime } from '@/components/relative-time'
import { Alert } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { RunStatusBadge } from '@/features/system/run-status'
import { t } from '@/i18n'
import { formatDateTime, formatDuration, parseDate } from '@/lib/format'
import { toast } from '@/lib/toast-store'
import { fetchCandidatesCsv, lifecycleRunQuery, type LifecycleRun } from './api'
import { CandidatesTable } from './candidates-table'
import { downloadTextFile } from './download'

/** Above this many items the report says that the table pages and the CSV is the full list. */
const LARGE_PLAN = 1000

function Stat({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="rounded-xl border border-transparent bg-card dark:border-border/60 px-4 py-3">
      <dt className="text-xs font-medium text-muted-foreground">{label}</dt>
      <dd className="mt-1 text-2xl font-semibold tabular-nums">{children}</dd>
    </div>
  )
}

function Summary({ summary }: { summary: NonNullable<LifecycleRun['summary']> }) {
  return (
    <dl
      className="grid grid-cols-2 gap-3 lg:grid-cols-4"
      aria-label={t('lifecycle.report.summary')}
    >
      <Stat label={t('lifecycle.report.stats.candidates')}>
        {summary.candidates.toLocaleString()}
      </Stat>
      <Stat label={t('lifecycle.report.stats.bytes')}>
        <BytesText bytes={summary.bytes} />
      </Stat>
      <Stat label={t('lifecycle.report.stats.repositories')}>
        {summary.repositories.toLocaleString()}
      </Stat>
      <Stat label={t('lifecycle.report.stats.protected')}>
        {summary.protected.toLocaleString()}
      </Stat>
    </dl>
  )
}

function Timing({ run }: { run: LifecycleRun }) {
  const started = parseDate(run.started_at)
  const finished = parseDate(run.finished_at)
  const seconds = started && finished ? (finished.getTime() - started.getTime()) / 1000 : null
  return (
    <p className="text-xs text-muted-foreground">
      <RelativeTime value={run.queued_at} />
      {seconds !== null ? ` - ${formatDuration(seconds)}` : ''}
      {run.report_expires_at
        ? ` - ${t('lifecycle.report.keptUntil', { date: formatDateTime(new Date(run.report_expires_at)) })}`
        : ''}
    </p>
  )
}

function ExportButton({ runId }: { runId: string }) {
  const [busy, setBusy] = useState(false)
  async function run(): Promise<void> {
    setBusy(true)
    try {
      downloadTextFile(`cleanup-plan-${runId}.csv`, await fetchCandidatesCsv(runId))
      toast.success(t('lifecycle.report.exported'))
    } catch (error) {
      toast.apiError(error, t('lifecycle.report.exportFailed'))
    } finally {
      setBusy(false)
    }
  }
  return (
    <Button variant="outline" size="sm" loading={busy} onClick={() => void run()}>
      <Download aria-hidden="true" />
      {busy ? t('lifecycle.report.exporting') : t('lifecycle.report.exportCsv')}
    </Button>
  )
}

export interface DryRunReportProps {
  runId: string
  /** The repositories offered as a filter, usually the ones the policy is attached to. */
  repositories?: readonly string[]
}

/**
 * The report of a cleanup run: status, summary first (counts and bytes), then the paged list
 * with the reason for each item, and an export of the whole plan. A run that is still planning
 * shows progress and refreshes itself; a failed or cancelled run says so and shows no stale
 * numbers; an expired report says when it was removed. The same component serves the preview
 * inside the policy editor and the run history.
 */
export function DryRunReport({ runId, repositories }: DryRunReportProps) {
  const query = useQuery(lifecycleRunQuery(runId))
  const [expired, setExpired] = useState(false)
  const run = query.data

  if (query.isError && !run) {
    return <ErrorPanel error={query.error} onRetry={() => void query.refetch()} />
  }
  if (!run) return <Skeleton className="h-40" aria-busy="true" />

  const active = run.status === 'queued' || run.status === 'running'
  const expiry = parseDate(run.report_expires_at)
  const gone = expired || (expiry !== null && expiry.getTime() < Date.now() && !active)
  const title = run.dry_run ? t('lifecycle.report.title') : t('lifecycle.report.titleReal')

  return (
    <section aria-labelledby={`report-${run.id}`} className="space-y-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="space-y-1">
          <h2
            id={`report-${run.id}`}
            className="flex flex-wrap items-center gap-2 text-base font-semibold"
          >
            {title}
            <RunStatusBadge status={run.status} />
            <Badge>{run.dry_run ? t('lifecycle.mode.dry') : t('lifecycle.mode.real')}</Badge>
          </h2>
          {run.policy_name ? (
            <p className="text-sm text-muted-foreground">
              {t('lifecycle.report.forPolicy', { name: run.policy_name })}
            </p>
          ) : null}
          <Timing run={run} />
        </div>
        {run.status === 'succeeded' && !gone && (run.summary?.candidates ?? 0) > 0 ? (
          <ExportButton runId={run.id} />
        ) : null}
      </div>

      {active ? (
        <Alert variant="info" title={t('lifecycle.report.planning')}>
          <span className="flex items-center gap-2">
            <Loader2 className="size-4 animate-spin-slow" aria-hidden="true" />
            {t('lifecycle.report.planningHint')}
          </span>
        </Alert>
      ) : null}
      {run.status === 'failed' ? (
        <Alert variant="destructive" title={t('lifecycle.report.failed')}>
          {t('lifecycle.report.failedDetail', { code: run.error_code ?? 'unknown' })}
        </Alert>
      ) : null}
      {run.status === 'cancelled' ? (
        <Alert variant="warning">{t('lifecycle.report.cancelled')}</Alert>
      ) : null}

      {run.status === 'succeeded' && gone ? (
        <Alert variant="warning" title={t('lifecycle.report.expired')}>
          {t('lifecycle.report.expiredDetail', {
            date: expiry ? formatDateTime(expiry) : '-',
          })}
        </Alert>
      ) : null}

      {run.status === 'succeeded' && !gone && run.summary ? (
        <>
          <Summary summary={run.summary} />
          {run.dry_run ? (
            <p className="text-sm text-muted-foreground">{t('lifecycle.report.nothingDeleted')}</p>
          ) : null}
          {run.summary.candidates > LARGE_PLAN ? (
            <Alert variant="info">{t('lifecycle.report.largeHint')}</Alert>
          ) : null}
          <Card>
            <CardContent className="pt-4">
              <h3 className="mb-2 text-sm font-semibold">{t('lifecycle.report.candidates')}</h3>
              <CandidatesTable
                runId={run.id}
                repositories={repositories}
                onExpired={() => setExpired(true)}
              />
            </CardContent>
          </Card>
        </>
      ) : null}
    </section>
  )
}
