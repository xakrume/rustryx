import { cn } from '@/lib/utils'

export interface MeterProps {
  /** 0 to 100. */
  percent: number
  /** Accessible name; the number is always shown as text by the caller too. */
  label: string
  className?: string
  /** `warning` from 90 percent and `error` at 100 tint the bar; the percentage text stays the cue. */
  tone?: 'accent' | 'warning' | 'destructive'
}

/**
 * A bar with the progressbar role and its value, for usage against a quota and for upload
 * progress. The width goes through the CSSOM (a `style` prop), which the strict CSP allows.
 */
export function Meter({ percent, label, className, tone = 'accent' }: MeterProps) {
  const value = Math.max(0, Math.min(100, Math.round(percent)))
  return (
    <div
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={value}
      className={cn('h-2 overflow-hidden rounded-full bg-muted', className)}
    >
      <div
        className={cn(
          'h-full transition-[width] duration-150',
          tone === 'accent' && 'bg-accent',
          tone === 'warning' && 'bg-warning',
          tone === 'destructive' && 'bg-destructive',
        )}
        style={{ width: `${value}%` }}
      />
    </div>
  )
}

/** The tint for a share of a quota: calm below 90 percent, a warning above, an error when full. */
export function quotaTone(percent: number): NonNullable<MeterProps['tone']> {
  return percent >= 100 ? 'destructive' : percent >= 90 ? 'warning' : 'accent'
}
