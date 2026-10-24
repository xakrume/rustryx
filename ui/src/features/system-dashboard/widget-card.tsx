import type { ReactNode } from 'react'
import { describeError } from '@/lib/describe-error'
import { ErrorPanel } from '@/components/error-panel'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { t } from '@/i18n'

export interface WidgetCardProps {
  title: string
  description?: string
  /** Extra controls next to the title. */
  actions?: ReactNode
  loading: boolean
  error: unknown
  onRetry: () => void
  /** The data is there: render it. A widget that failed AFTER it had data keeps the old data. */
  hasData: boolean
  children: ReactNode
  className?: string
}

/**
 * One dashboard card with its own loading, error and forbidden states. A widget that cannot
 * load shows why inside its own card (with the error code and request id) and leaves its
 * neighbours and the page alone: the dashboard never breaks because one number is missing.
 */
export function WidgetCard({
  title,
  description,
  actions,
  loading,
  error,
  onRetry,
  hasData,
  children,
  className,
}: WidgetCardProps) {
  const failed = error !== null && error !== undefined && !hasData
  const forbidden = failed && describeError(error).kind === 'forbidden'
  return (
    <Card className={className}>
      <CardHeader className="flex-row items-start justify-between gap-2">
        <div className="min-w-0 space-y-1">
          <CardTitle>{title}</CardTitle>
          {description ? <CardDescription>{description}</CardDescription> : null}
        </div>
        {actions}
      </CardHeader>
      <CardContent>
        {forbidden ? (
          <p className="text-sm text-muted-foreground">{t('systemWidgets.forbidden')}</p>
        ) : failed ? (
          <ErrorPanel error={error} onRetry={onRetry} title={t('systemWidgets.loadFailed')} />
        ) : loading && !hasData ? (
          <Skeleton className="h-32" aria-busy="true" />
        ) : (
          children
        )}
      </CardContent>
    </Card>
  )
}
