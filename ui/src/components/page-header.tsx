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
    <header className={cn('mb-8 flex flex-wrap items-end justify-between gap-4', className)}>
      <div className="min-w-0">
        <h1
          data-page-title=""
          tabIndex={-1}
          className="text-[1.75rem] leading-tight font-semibold tracking-[-0.025em] text-foreground outline-none md:text-[2rem]"
        >
          {title}
        </h1>
        {description ? (
          <p className="mt-1.5 max-w-2xl text-[0.9375rem] leading-6 text-muted-foreground">
            {description}
          </p>
        ) : null}
      </div>
      {actions ? <div className="flex flex-wrap items-center gap-2">{actions}</div> : null}
    </header>
  )
}
