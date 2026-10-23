import type { ComponentProps } from 'react'
import { formatBytes, formatExactBytes } from '@/lib/format'
import { cn } from '@/lib/utils'

export interface BytesTextProps extends Omit<ComponentProps<'span'>, 'children'> {
  bytes: number | null | undefined
}

/** A byte count in binary units with the exact number as tooltip. Tabular digits keep columns aligned. */
export function BytesText({ bytes, className, ...props }: BytesTextProps) {
  const known = typeof bytes === 'number' && Number.isFinite(bytes) && bytes >= 0
  return (
    <span
      className={cn('tabular-nums', className)}
      title={known ? formatExactBytes(bytes) : undefined}
      {...props}
    >
      {formatBytes(bytes)}
    </span>
  )
}
