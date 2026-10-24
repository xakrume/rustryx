import { ArrowDownToLine, HardDrive, Layers, type LucideIcon } from 'lucide-react'
import { StatusBadge, type StatusKind } from '@/components/status-badge'
import { Badge } from '@/components/ui/badge'
import { t } from '@/i18n'
import type { RepositoryView } from './api'

const KNOWN_FORMATS = ['maven2', 'npm', 'pypi', 'docker', 'oci', 'helm', 'raw', 'pub'] as const

/** Display name of a format. An unknown label (a format added later) is shown as it is sent. */
export function formatLabel(format: string): string {
  return (KNOWN_FORMATS as readonly string[]).includes(format)
    ? t(`repos.format.${format as (typeof KNOWN_FORMATS)[number]}`)
    : format
}

export function FormatBadge({ format }: { format: string }) {
  return <Badge variant="neutral">{formatLabel(format)}</Badge>
}

const TYPE_ICON: Record<RepositoryView['repo_type'], LucideIcon> = {
  hosted: HardDrive,
  proxy: ArrowDownToLine,
  group: Layers,
}

/** Hosted, proxy or group: an icon and the word, so the type never rests on shape alone. */
export function RepoTypeBadge({ type }: { type: RepositoryView['repo_type'] }) {
  const Icon = TYPE_ICON[type]
  return (
    <Badge variant="neutral">
      <Icon aria-hidden="true" />
      {t(`repos.type.${type}`)}
    </Badge>
  )
}

export function OnlineBadge({ online }: { online: boolean }) {
  return online ? (
    <StatusBadge status="ok" label={t('status.online')} />
  ) : (
    <StatusBadge status="warning" label={t('status.offline')} />
  )
}

const UPSTREAM_STATUS: Record<string, StatusKind> = {
  ok: 'ok',
  degraded: 'warning',
  blocked: 'warning',
  auto_blocked: 'error',
  unknown: 'unknown',
}

export function UpstreamBadge({ state }: { state: string }) {
  const key = state in UPSTREAM_STATUS ? state : 'unknown'
  return (
    <StatusBadge
      status={UPSTREAM_STATUS[key] ?? 'unknown'}
      label={t(
        `repos.upstream.${key as 'ok' | 'degraded' | 'blocked' | 'auto_blocked' | 'unknown'}`,
      )}
    />
  )
}
