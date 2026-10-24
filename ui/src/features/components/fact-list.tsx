import type { ReactNode } from 'react'
import { cn } from '@/lib/utils'

/**
 * A definition list of facts: a muted term and its value, two columns from the sm breakpoint and
 * stacked on a phone. Values wrap instead of overflowing (long paths and digests break anywhere),
 * so no fact list ever widens the page.
 */
export function FactList({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <dl
      className={cn(
        'grid items-baseline gap-x-4 gap-y-2 text-sm sm:grid-cols-[minmax(7rem,max-content)_minmax(0,1fr)]',
        className,
      )}
    >
      {children}
    </dl>
  )
}

export function Fact({
  label,
  children,
  mono = false,
}: {
  label: string
  children: ReactNode
  /** Technical values (paths, digests, ids) in the code font. */
  mono?: boolean
}) {
  return (
    <div className="contents">
      <dt className="text-muted-foreground ">{label}</dt>
      <dd className={cn('min-w-0 break-words', mono && 'font-mono text-[0.8125rem]')}>
        {children}
      </dd>
    </div>
  )
}
