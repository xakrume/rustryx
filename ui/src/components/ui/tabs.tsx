import * as TabsPrimitive from '@radix-ui/react-tabs'
import type { ComponentProps } from 'react'
import { cn } from '@/lib/utils'

export const Tabs = TabsPrimitive.Root

export function TabsList({ className, ...props }: ComponentProps<typeof TabsPrimitive.List>) {
  return (
    <TabsPrimitive.List
      className={cn('inline-flex items-center gap-2 border-b border-border', className)}
      {...props}
    />
  )
}

/** The selected tab has a bar under it AND the aria-selected state: never color alone. */
export function TabsTrigger({ className, ...props }: ComponentProps<typeof TabsPrimitive.Trigger>) {
  return (
    <TabsPrimitive.Trigger
      className={cn(
        '-mb-px inline-flex h-[calc(var(--control-h)+0.25rem)] items-center border-b-2 border-transparent px-2 text-sm font-medium whitespace-nowrap text-muted-foreground transition-colors duration-200 hover:text-foreground data-[state=active]:border-foreground data-[state=active]:text-foreground',
        className,
      )}
      {...props}
    />
  )
}

export function TabsContent({ className, ...props }: ComponentProps<typeof TabsPrimitive.Content>) {
  return <TabsPrimitive.Content className={cn('pt-5', className)} {...props} />
}
