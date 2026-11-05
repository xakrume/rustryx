import { CloudOff, SearchX, ServerCrash, TriangleAlert } from 'lucide-react'
import { CopyButton } from '@/components/copy-button'
import { ForbiddenPanel } from '@/components/forbidden-panel'
import { Button } from '@/components/ui/button'
import { useCountdown } from '@/hooks/use-countdown'
import { t } from '@/i18n'
import { describeError } from '@/lib/describe-error'
import { cn } from '@/lib/utils'

export interface ErrorPanelProps {
  error: unknown
  /** Shown as a Retry button. Disabled while a Retry-After countdown runs. */
  onRetry?: () => void
  /** Overrides the title derived from the error kind, for example "The list could not be loaded". */
  title?: string
  className?: string
}

/**
 * How a failed load is shown anywhere: a title by error kind, the server's own safe message,
 * the stable error `code` and the `request_id` each with a copy button (so a bug report can
 * quote them), and a Retry button that waits out a Retry-After. A 403 becomes the forbidden
 * panel, because "retry" is the wrong advice there.
 */
export function ErrorPanel({ error, onRetry, title, className }: ErrorPanelProps) {
  const d = describeError(error)
  const secondsLeft = useCountdown(d.retryAt)

  if (d.kind === 'forbidden' || d.kind === 'csrf') {
    return <ForbiddenPanel className={className} />
  }

  const Icon =
    d.kind === 'network'
      ? CloudOff
      : d.kind === 'not_found'
        ? SearchX
        : d.kind === 'server' || d.kind === 'unavailable'
          ? ServerCrash
          : TriangleAlert
  const message = d.kind === 'not_found' ? t('errors.detail.notFound') : d.message

  return (
    <div
      role="alert"
      className={cn(
        'flex gap-3 rounded-xl border border-transparent bg-card dark:border-border/60 p-4 text-sm',
        className,
      )}
    >
      <Icon className="mt-0.5 size-5 shrink-0 text-destructive-text" aria-hidden="true" />
      <div className="min-w-0 flex-1 space-y-2">
        <div>
          <h2 className="text-base font-semibold">{title ?? d.title}</h2>
          <p className="text-muted-foreground">{message}</p>
        </div>
        {d.code || d.requestId ? (
          <ul className="space-y-1 text-xs text-muted-foreground">
            {d.code ? (
              <li className="flex flex-wrap items-center gap-1">
                <span>{t('common.errorCode')}</span>
                <code className="font-mono text-foreground">{d.code}</code>
                <CopyButton
                  value={d.code}
                  what={t('common.errorCode')}
                  size="icon"
                  className="size-6"
                />
              </li>
            ) : null}
            {d.requestId ? (
              <li className="flex flex-wrap items-center gap-1">
                <span>{t('common.requestId')}</span>
                <code className="font-mono break-all text-foreground">{d.requestId}</code>
                <CopyButton
                  value={d.requestId}
                  what={t('common.requestId')}
                  size="icon"
                  className="size-6"
                />
              </li>
            ) : null}
          </ul>
        ) : null}
        {onRetry ? (
          <Button variant="outline" size="sm" onClick={onRetry} disabled={secondsLeft > 0}>
            {secondsLeft > 0 ? t('errors.retryIn', { seconds: secondsLeft }) : t('common.retry')}
          </Button>
        ) : null}
      </div>
    </div>
  )
}
