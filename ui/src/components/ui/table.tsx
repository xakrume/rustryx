import type { ComponentProps } from 'react'
import { cn } from '@/lib/utils'

export interface TableProps extends ComponentProps<'table'> {
  /** Accessible name of the scroll region around the table. */
  label: string
  /** Class of the scroll container, for example a max height that makes the header sticky. */
  containerClassName?: string
}

/**
 * The table sits in a focusable scroll region: on a narrow screen it scrolls sideways instead
 * of widening the page, and tabIndex 0 lets keyboard users scroll it (WCAG 2.1.1). A max
 * height on the container (containerClassName) makes the sticky header work, because sticky
 * positions against the nearest scroll container.
 */
export function Table({ className, label, containerClassName, ...props }: TableProps) {
  return (
    <div
      role="region"
      aria-label={label}
      // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- keyboard users must be able to scroll the region
      tabIndex={0}
      className={cn(
        'relative w-full overflow-auto rounded-lg border border-border/70 bg-card',
        containerClassName,
      )}
    >
      <table
        className={cn(
          'w-full caption-bottom border-collapse text-(length:--text-table) leading-(--text-table-line)',
          className,
        )}
        {...props}
      />
    </div>
  )
}

export function TableHeader({ className, ...props }: ComponentProps<'thead'>) {
  return <thead className={cn('bg-card [&_tr]:border-b', className)} {...props} />
}

export function TableBody({ className, ...props }: ComponentProps<'tbody'>) {
  return <tbody className={cn('[&_tr:last-child]:border-0', className)} {...props} />
}

export function TableRow({ className, ...props }: ComponentProps<'tr'>) {
  return (
    <tr
      className={cn(
        'h-(--row-h) border-b border-border/70 transition-colors duration-150 hover:bg-muted data-[state=selected]:bg-info-subtle',
        className,
      )}
      {...props}
    />
  )
}

export function TableHead({ className, ...props }: ComponentProps<'th'>) {
  return (
    <th
      className={cn(
        // Sticky on the th, not the thead: that is the form every browser honours.
        'sticky top-0 z-10 bg-card px-(--cell-px) text-left align-middle text-xs font-medium whitespace-nowrap text-muted-foreground',
        className,
      )}
      {...props}
    />
  )
}

export function TableCell({ className, ...props }: ComponentProps<'td'>) {
  return <td className={cn('px-(--cell-px) align-middle', className)} {...props} />
}

export function TableCaption({ className, ...props }: ComponentProps<'caption'>) {
  return <caption className={cn('py-2 text-sm text-muted-foreground', className)} {...props} />
}
