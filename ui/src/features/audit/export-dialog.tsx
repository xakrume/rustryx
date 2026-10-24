import { useEffect, useRef, useState } from 'react'
import { isAbortError, isApiError } from '@/api/errors'
import { ErrorPanel } from '@/components/error-panel'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { t } from '@/i18n'
import type { AuditFilters, TimeBounds } from './api'
import {
  lineGate,
  openExportStream,
  openSink,
  SaveDeclined,
  type ExportProgress,
  type ExportSink,
} from './export'
import { isoToLocalInput, localInputToIso } from './time-input'

const MAX_DAYS = 366
type Phase = 'idle' | 'running' | 'interrupted' | 'done' | 'failed'

export interface ExportDialogProps {
  filters: AuditFilters
  /** The time range of the table, used as the starting point of the export range. */
  bounds: TimeBounds
  onClose: () => void
}

/** Filters that narrow the export besides the time range, as "actor: alice". */
function activeFilters(filters: AuditFilters): string[] {
  return (
    [
      ['kind', filters.kind],
      ['actor', filters.actor],
      ['action', filters.action],
      ['target', filters.target],
      ['repository', filters.repository],
      ['project', filters.project],
      ['request_id', filters.request_id],
      ['outcome', filters.outcome],
    ] as const
  )
    .filter(([, value]) => value !== undefined && value !== '')
    .map(([name, value]) => `${name}: ${value}`)
}

/**
 * Exports the matching events as NDJSON. The bytes stream from the server to a file (or, where the
 * browser cannot write to a file, to a memory copy that is downloaded at the end); the page never
 * parses the export into one big value. A connection that breaks leaves a file with whole events
 * only and a Resume that continues after the last one (`after=<cursor>`).
 */
export function ExportDialog({ filters, bounds, onClose }: ExportDialogProps) {
  const [from, setFrom] = useState(isoToLocalInput(bounds.from))
  const [to, setTo] = useState(isoToLocalInput(bounds.to))
  const [phase, setPhase] = useState<Phase>('idle')
  const [progress, setProgress] = useState<ExportProgress>({ count: 0, cursor: null })
  const [error, setError] = useState<unknown>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [savedAs, setSavedAs] = useState<string | null>(null)
  const sink = useRef<ExportSink | null>(null)
  const progressRef = useRef<ExportProgress>({ count: 0, cursor: null })
  const controller = useRef<AbortController | null>(null)

  // Closing the dialog gives up an export that is still open: the file is aborted, memory dropped.
  useEffect(
    () => () => {
      controller.current?.abort()
      void sink.current?.abort()
    },
    [],
  )

  const fromIso = localInputToIso(from)
  const toIso = localInputToIso(to)
  const spanDays =
    fromIso !== null && toIso !== null
      ? (Date.parse(toIso) - Date.parse(fromIso)) / 86_400_000
      : null
  const rangeError =
    spanDays !== null && spanDays <= 0
      ? t('auditLog.export.rangeInvalid')
      : spanDays !== null && spanDays > MAX_DAYS
        ? t('auditLog.export.rangeTooLarge', { days: MAX_DAYS })
        : null

  async function run(resume: boolean): Promise<void> {
    setPhase('running')
    setError(null)
    setNotice(null)
    const abort = new AbortController()
    controller.current = abort
    const start = resume ? progressRef.current : { count: 0, cursor: null }
    progressRef.current = start
    setProgress(start)
    try {
      if (sink.current === null) sink.current = await openSink(t('auditLog.export.fileName'))
      const opened = await openExportStream({
        filters,
        bounds: { from: fromIso ?? undefined, to: toIso ?? undefined },
        after: start.cursor,
        signal: abort.signal,
      })
      const gated = opened.stream.pipeThrough(
        lineGate(start, (next) => {
          progressRef.current = next
          setProgress(next)
        }),
      )
      await sink.current.write(gated, abort.signal)
      const fileName = opened.fileName ?? t('auditLog.export.fileName')
      await sink.current.finish(fileName)
      sink.current = null
      setSavedAs(fileName)
      setPhase('done')
    } catch (caught) {
      if (caught instanceof SaveDeclined) {
        setNotice(t('auditLog.export.saveDeclined'))
        setPhase('idle')
      } else if (isApiError(caught) && caught.status !== 0) {
        // The server refused (range too large, forbidden, throttled): nothing was written.
        void sink.current?.abort()
        sink.current = null
        setError(caught)
        setPhase('failed')
      } else {
        // Cut connection, a stopped export: resumable from the last complete event.
        setError(isAbortError(caught) ? null : caught)
        setPhase('interrupted')
      }
    }
  }

  function close(): void {
    controller.current?.abort()
    onClose()
  }

  const filtersShown = activeFilters(filters)
  const running = phase === 'running'
  return (
    <Dialog open onOpenChange={(open) => !open && close()}>
      <DialogContent className="max-w-xl" dismissible={!running}>
        <DialogHeader>
          <DialogTitle>{t('auditLog.export.title')}</DialogTitle>
          <DialogDescription>{t('auditLog.export.description')}</DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <div className="grid gap-3 sm:grid-cols-2">
            <div className="space-y-1">
              <Label htmlFor="export-from">{t('auditLog.export.from')}</Label>
              <Input
                id="export-from"
                type="datetime-local"
                value={from}
                disabled={running || phase === 'interrupted'}
                onChange={(e) => setFrom(e.target.value)}
              />
            </div>
            <div className="space-y-1">
              <Label htmlFor="export-to">{t('auditLog.export.to')}</Label>
              <Input
                id="export-to"
                type="datetime-local"
                value={to}
                disabled={running || phase === 'interrupted'}
                aria-invalid={rangeError ? true : undefined}
                aria-describedby="export-range-help"
                onChange={(e) => setTo(e.target.value)}
              />
            </div>
          </div>
          <p
            id="export-range-help"
            className={
              rangeError
                ? 'text-xs font-medium text-destructive-text'
                : 'text-xs text-muted-foreground'
            }
          >
            {rangeError ?? t('auditLog.export.rangeHelper')}
          </p>
          <div className="space-y-1 text-sm">
            <p className="font-medium">{t('auditLog.export.filters')}</p>
            {filtersShown.length === 0 ? (
              <p className="text-muted-foreground">{t('auditLog.export.noFilters')}</p>
            ) : (
              <ul className="flex flex-wrap gap-1.5">
                {filtersShown.map((entry) => (
                  <li
                    key={entry}
                    className="rounded-md border border-border bg-muted px-2 py-0.5 font-mono text-xs"
                  >
                    {entry}
                  </li>
                ))}
              </ul>
            )}
          </div>
          <div role="status" aria-live="polite" className="space-y-2 text-sm">
            {running || phase === 'interrupted' ? (
              <p className="font-medium">
                {running ? t('auditLog.export.running') : null}{' '}
                {t('auditLog.export.progress', { count: progress.count })}
              </p>
            ) : null}
            {phase === 'done' ? (
              <Alert variant="success">
                {t('auditLog.export.done', { count: progress.count, file: savedAs ?? '' })}
              </Alert>
            ) : null}
            {phase === 'interrupted' ? (
              <Alert variant="warning">
                {t('auditLog.export.interrupted', { count: progress.count })}
              </Alert>
            ) : null}
            {notice ? <Alert>{notice}</Alert> : null}
          </div>
          {phase === 'failed' || (phase === 'interrupted' && error !== null) ? (
            <ErrorPanel error={error} title={t('auditLog.export.failed')} />
          ) : null}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={close}>
            {t('common.close')}
          </Button>
          {running ? (
            <Button variant="outline" onClick={() => controller.current?.abort()}>
              {t('auditLog.export.cancel')}
            </Button>
          ) : phase === 'interrupted' ? (
            <Button onClick={() => void run(true)}>{t('auditLog.export.resume')}</Button>
          ) : (
            <Button disabled={rangeError !== null} onClick={() => void run(false)}>
              {phase === 'done' ? t('auditLog.export.again') : t('auditLog.export.start')}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
