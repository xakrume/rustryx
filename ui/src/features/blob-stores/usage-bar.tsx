import { StatusBadge, type StatusKind } from '@/components/status-badge'
import { t } from '@/i18n'
import { formatBytes } from '@/lib/format'
import { cn } from '@/lib/utils'
import type { BlobStoreView } from './api'
import { usageDisplay, type QuotaState } from './form-model'

const HEALTH_KIND: Record<string, StatusKind> = {
  ok: 'ok',
  degraded: 'warning',
  unreachable: 'error',
  unknown: 'unknown',
}

/** Health as icon plus text. The last error code is part of the text when there is one. */
export function HealthBadge({ health }: { health: BlobStoreView['health'] }) {
  const state = health.state in HEALTH_KIND ? health.state : 'unknown'
  return (
    <StatusBadge status={HEALTH_KIND[state] ?? 'unknown'} label={t(`blobStores.health.${state}`)} />
  )
}

/** Quota status in words: never only a colored bar. `ok` and `none` stay quiet in the list. */
export function QuotaBadge({ quota, quiet = false }: { quota: QuotaState; quiet?: boolean }) {
  if (quota === 'near' || quota === 'over') {
    return (
      <StatusBadge
        status="warning"
        label={t(quota === 'over' ? 'blobStores.quota.over' : 'blobStores.quota.near')}
      />
    )
  }
  if (quiet) return null
  return (
    <StatusBadge
      status={quota === 'ok' ? 'ok' : 'neutral'}
      label={t(quota === 'ok' ? 'blobStores.quota.ok' : 'blobStores.quota.none')}
    />
  )
}

const TONE: Record<QuotaState, string> = {
  ok: 'bg-success',
  near: 'bg-warning',
  over: 'bg-warning',
  none: 'bg-info',
}

export interface UsageBarProps {
  view: Pick<BlobStoreView, 'name' | 'usage' | 'quota_bytes'>
  className?: string
}

/**
 * Used space against the soft quota, or against the disk when the store has no quota. The bar
 * is a `meter` with a text value; the numbers and the quota badge say the same in words, so the
 * fill color is a third cue. Passing the soft quota is amber and never blocking text: the
 * quota only raises alerts (contract: `quota_bytes` is soft).
 */
export function UsageBar({ view, className }: UsageBarProps) {
  const display = usageDisplay(view)
  const used = view.usage?.used_bytes
  if (used === undefined) {
    return (
      <span className={cn('text-sm text-muted-foreground', className)}>
        {t('blobStores.usage.unknown')}
      </span>
    )
  }
  const usedText = formatBytes(used)
  if (display.percent === null || display.limitBytes === null) {
    return (
      <span className={cn('text-sm tabular-nums', className)}>
        {t('blobStores.usage.noLimit', { used: usedText })}
      </span>
    )
  }
  const limitText = formatBytes(display.limitBytes)
  const summary = t(
    display.basis === 'quota' ? 'blobStores.usage.ofQuota' : 'blobStores.usage.ofDisk',
    {
      used: usedText,
      limit: limitText,
    },
  )
  return (
    <div className={cn('min-w-48 space-y-1', className)}>
      <div
        role="meter"
        aria-label={t('blobStores.usage.meterLabel', { name: view.name })}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={display.percent}
        aria-valuetext={t('blobStores.usage.meterText', {
          used: usedText,
          limit: limitText,
          percent: display.percent,
        })}
        className="h-2 w-full overflow-hidden rounded-full border border-input bg-muted"
      >
        {/* The width is data, so it cannot be a class; a style property is set through the
            CSSOM, which the strict style-src does not block. */}
        <div
          className={cn('h-full rounded-full', TONE[display.quota])}
          style={{ width: `${display.percent}%` }}
        />
      </div>
      <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground tabular-nums">
        <span>{summary}</span>
        <span className="font-medium">{display.percent}%</span>
      </p>
    </div>
  )
}
