import type { ReactNode } from 'react'
import { usePageTitle } from '@/hooks/use-page-title'
import { cn } from '@/lib/utils'

export interface PageHeaderProps {
  title: string
  description?: ReactNode
  /** Primary and secondary actions, right aligned on wide screens. */
  actions?: ReactNode
  className?: string
}

/**
 * The page's one <h1>. It is focusable (tabIndex -1) and marked data-page-title: after a route
 * change the shell moves focus here and announces the title, so keyboard and screen reader
 * users land at the top of the new page instead of on a dead link (WCAG 2.4.3).
 */
export function PageHeader({ title, description, actions, className }: PageHeaderProps) {
  usePageTitle(title)
  return (
    <header className={cn('mb-4 flex flex-wrap items-start justify-between gap-3', className)}>
      <div className="min-w-0">
        <h1
          data-page-title=""
          tabIndex={-1}
          className="text-xl leading-7 font-semibold text-foreground outline-none"
        >
          {title}
        </h1>
        {description ? <p className="mt-1 text-sm text-muted-foreground">{description}</p> : null}
      </div>
      {actions ? <div className="flex flex-wrap items-center gap-2">{actions}</div> : null}
    </header>
  )
}
