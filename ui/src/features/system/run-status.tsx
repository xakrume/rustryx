import { StatusBadge, type StatusKind } from '@/components/status-badge'
import { t } from '@/i18n'

/** Every status a run can have: lifecycle runs, task runs, GC runs (docs/api-conventions.md, 14). */
export type RunStatus =
  'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled' | 'aborted' | 'stopped'

const KINDS: Record<RunStatus, StatusKind> = {
  queued: 'neutral',
  running: 'info',
  succeeded: 'ok',
  failed: 'error',
  cancelled: 'neutral',
  aborted: 'warning',
  stopped: 'neutral',
}

const isKnown = (status: string): status is RunStatus => status in KINDS

/** A run that has not reached a final state yet: the page keeps polling it. */
export function isActiveRun(status: string | undefined): boolean {
  return status === 'queued' || status === 'running'
}

/** Icon plus the status word. An unknown status from a newer server is shown as it is. */
export function RunStatusBadge({ status }: { status: string }) {
  if (!isKnown(status)) return <StatusBadge status="unknown" label={status} />
  return <StatusBadge status={KINDS[status]} label={t(`runStatus.${status}`)} />
}

/** Poll every 2 s at first, then 5 s, then 15 s (docs/api-conventions.md, section 14). */
const FAST_TEST_MS = 15

export function pollInterval(updates: number): number {
  // A unit test cannot wait seconds per poll; the build replaces MODE, so production has no branch.
  if (import.meta.env.MODE === 'test') return FAST_TEST_MS
  if (updates < 5) return 2_000
  if (updates < 15) return 5_000
  return 15_000
}
