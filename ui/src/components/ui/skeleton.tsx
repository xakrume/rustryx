import type { ComponentProps } from 'react'
import { cn } from '@/lib/utils'

/** Decorative placeholder; the surrounding region carries aria-busy and a text status. */
export function Skeleton({ className, ...props }: ComponentProps<'div'>) {
  return (
    <div
      aria-hidden="true"
      className={cn('animate-pulse-soft rounded-md bg-muted', className)}
      {...props}
    />
  )
}
