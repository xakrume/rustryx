import type { LucideIcon } from 'lucide-react'
import type { ReactNode } from 'react'
import { cn } from '@/lib/utils'

export interface EmptyStateProps {
  title: string
  description?: ReactNode
  icon?: LucideIcon
  /** One primary call to action (docs/ui-page-inventory.md, section 5). */
  action?: ReactNode
  className?: string
}

/** Text first, one action, no illustration: the page says what is missing and what to do. */
export function EmptyState({ title, description, icon: Icon, action, className }: EmptyStateProps) {
  return (
    <div
      className={cn(
        'flex flex-col items-center gap-2 rounded-xl border border-transparent bg-card px-6 py-14 text-center dark:border-border/60',
        className,
      )}
    >
      {Icon ? (
        <Icon
          className="mb-1 size-10 text-muted-foreground"
          strokeWidth={1.25}
          aria-hidden="true"
        />
      ) : null}
      <h2 className="text-lg font-semibold">{title}</h2>
      {description ? <p className="max-w-md text-sm text-muted-foreground">{description}</p> : null}
      {action ? <div className="mt-2">{action}</div> : null}
    </div>
  )
}
