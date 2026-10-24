import type { ComponentProps } from 'react'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { useNow } from '@/hooks/use-now'
import { formatDateTime, formatRelative, parseDate } from '@/lib/format'
import { cn } from '@/lib/utils'

export interface RelativeTimeProps extends Omit<ComponentProps<'time'>, 'children' | 'dateTime'> {
  /** RFC 3339 instant from the API, or null for "never". */
  value: string | null | undefined
  /** Make the element a tab stop so keyboard users can open the tooltip. Off in dense tables. */
  focusable?: boolean
}

/**
 * "5 minutes ago" with the absolute local time and the UTC instant in a tooltip. The absolute
 * time is also in the DOM for screen readers (visually hidden) because a tooltip alone is not
 * reachable without a pointer. One shared ticker (useNow) refreshes every instance.
 */
export function RelativeTime({ value, focusable = false, className, ...props }: RelativeTimeProps) {
  const now = useNow()
  const date = parseDate(value)
  if (date === null) return <span className={className}>-</span>
  const absolute = formatDateTime(date)
  // The shared ticker knows only the start of the current minute, so an instant from the last
  // seconds lies "ahead" of it. A moment that has already happened is never shown as "in 40 seconds".
  const reference = date.getTime() > now.getTime() && date.getTime() <= Date.now() ? date : now
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <time
          dateTime={date.toISOString()}
          // A tab stop only when the caller asks (focusable): then keyboard users can open the tooltip.
          // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- opt in, for tooltip access
          tabIndex={focusable ? 0 : undefined}
          className={cn('whitespace-nowrap', className)}
          {...props}
        >
          {formatRelative(date, reference)}
          <span className="sr-only"> ({absolute})</span>
        </time>
      </TooltipTrigger>
      <TooltipContent>
        <div>{absolute}</div>
        <div className="font-mono text-muted-foreground">{date.toISOString()}</div>
      </TooltipContent>
    </Tooltip>
  )
}
