import {
  CircleCheck,
  CircleDashed,
  Info,
  ShieldAlert,
  TriangleAlert,
  type LucideIcon,
} from 'lucide-react'
import { useId } from 'react'
import { RelativeTime } from '@/components/relative-time'
import { Badge } from '@/components/ui/badge'
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover'
import { t } from '@/i18n'
import { cn } from '@/lib/utils'
import type { RepositoryView } from './api'

type Health = NonNullable<RepositoryView['health']>
type Severity = 'critical' | 'severe' | 'moderate'

interface SeverityStyle {
  icon: LucideIcon
  badge: 'destructive' | 'warning' | 'neutral'
  dot: string
}

const SEVERITIES: readonly Severity[] = ['critical', 'severe', 'moderate']

const STYLE: Record<Severity, SeverityStyle> = {
  critical: { icon: ShieldAlert, badge: 'destructive', dot: 'bg-destructive' },
  severe: { icon: TriangleAlert, badge: 'warning', dot: 'bg-warning' },
  moderate: { icon: Info, badge: 'neutral', dot: 'bg-input' },
}

/** The most serious level that has findings, or null when the scan found none. */
function worstSeverity(health: Health): Severity | null {
  return SEVERITIES.find((s) => health[s] > 0) ?? null
}

function isPartial(health: Health): boolean {
  return health.scanned_components < health.component_count
}

/** One sentence that says everything the cell shows: the accessible name of the trigger. */
function summary(health: Health): string {
  const total = health.critical + health.severe + health.moderate
  const findings =
    total === 0
      ? t('repos.health.summaryNone')
      : t('repos.health.summary', {
          critical: health.critical,
          severe: health.severe,
          moderate: health.moderate,
        })
  const coverage = t('repos.health.coverage', {
    scanned: health.scanned_components,
    total: health.component_count,
  })
  return `${findings} ${coverage}`
}

function CellFace({ health }: { health: Health }) {
  const worst = worstSeverity(health)
  const partial = isPartial(health)
  if (worst === null) {
    return partial ? (
      <Badge variant="neutral">
        <CircleDashed aria-hidden="true" />
        {t('repos.health.partial')}
      </Badge>
    ) : (
      <Badge variant="success">
        <CircleCheck aria-hidden="true" />
        {t('repos.health.noIssues')}
      </Badge>
    )
  }
  const { icon: Icon, badge } = STYLE[worst]
  const rest = health.critical + health.severe + health.moderate - health[worst]
  return (
    <>
      <Badge variant={badge}>
        <Icon aria-hidden="true" />
        {t(`repos.health.worst.${worst}`, { count: health[worst] })}
      </Badge>
      {rest > 0 ? (
        <span className="text-muted-foreground tabular-nums">
          {t('repos.health.more', { count: rest })}
        </span>
      ) : null}
      {partial ? (
        <span className="text-muted-foreground">· {t('repos.health.partialShort')}</span>
      ) : null}
    </>
  )
}

function Breakdown({ health, titleId }: { health: Health; titleId: string }) {
  return (
    <div className="space-y-3 text-sm">
      <h3 id={titleId} className="font-semibold">
        {t('repos.health.title')}
      </h3>
      <dl className="grid grid-cols-[1fr_auto] gap-x-6 gap-y-1.5">
        {SEVERITIES.map((s) => (
          <div key={s} className="contents">
            <dt className="flex items-center gap-2">
              <span className={cn('size-2 rounded-full', STYLE[s].dot)} aria-hidden="true" />
              {t(`repos.health.${s}`)}
            </dt>
            <dd className="text-right font-medium tabular-nums">{health[s].toLocaleString()}</dd>
          </div>
        ))}
      </dl>
      <div className="space-y-0.5 border-t border-border/70 pt-3 text-muted-foreground">
        <p>
          {t('repos.health.coverage', {
            scanned: health.scanned_components.toLocaleString(),
            total: health.component_count.toLocaleString(),
          })}
        </p>
        {health.scanned_at ? (
          <p>
            {t('repos.health.lastScan')}: <RelativeTime value={health.scanned_at} />
          </p>
        ) : null}
      </div>
    </div>
  )
}

/**
 * The vulnerability column of the repository list. The cell names the worst severity in words
 * ("2 critical") and counts the rest ("+165"); a click opens the full breakdown and how much of
 * the repository was scanned. The trigger's accessible name is the whole summary, so a screen
 * reader user gets everything without opening it.
 */
export function HealthCell({ health }: { health: RepositoryView['health'] }) {
  const titleId = useId()
  if (!health?.scanned) {
    return <span className="text-muted-foreground">{t('repos.health.notScanned')}</span>
  }
  return (
    <Popover>
      <PopoverTrigger
        aria-label={summary(health)}
        className="focus-inset -mx-1 inline-flex items-center gap-1.5 rounded-full px-1 py-0.5 hover:bg-foreground/[0.05]"
      >
        <CellFace health={health} />
      </PopoverTrigger>
      <PopoverContent aria-labelledby={titleId} className="w-64">
        <Breakdown health={health} titleId={titleId} />
      </PopoverContent>
    </Popover>
  )
}
