import { Command as CommandPrimitive } from 'cmdk'
import { Search } from 'lucide-react'
import type { ComponentProps, ReactNode } from 'react'
import { t } from '@/i18n'
import { cn } from '@/lib/utils'
import { Dialog, DialogContent, DialogDescription, DialogTitle } from './dialog'

export function Command({ className, ...props }: ComponentProps<typeof CommandPrimitive>) {
  return (
    <CommandPrimitive
      className={cn(
        'flex h-full w-full flex-col overflow-hidden text-popover-foreground',
        className,
      )}
      {...props}
    />
  )
}

export interface CommandDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  title: string
  description: string
  /** Accessible name of the search box. cmdk names the input through the Command label. */
  inputLabel: string
  children: ReactNode
}

/** The command palette shell: a dialog (focus trap, Escape) around cmdk's combobox. */
export function CommandDialog({
  open,
  onOpenChange,
  title,
  description,
  inputLabel,
  children,
}: CommandDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        hideCloseButton
        className="top-[15%] max-h-[70dvh] max-w-2xl translate-y-0 gap-0 overflow-hidden bg-popover/95 p-0 backdrop-blur-2xl"
      >
        <DialogTitle className="sr-only">{title}</DialogTitle>
        <DialogDescription className="sr-only">{description}</DialogDescription>
        <Command
          loop
          label={inputLabel}
          className="[&_[cmdk-group-heading]]:px-3 [&_[cmdk-group-heading]]:py-1.5 [&_[cmdk-group-heading]]:text-xs [&_[cmdk-group-heading]]:font-medium [&_[cmdk-group-heading]]:text-muted-foreground"
        >
          {children}
        </Command>
      </DialogContent>
    </Dialog>
  )
}

export function CommandInput({
  className,
  ...props
}: ComponentProps<typeof CommandPrimitive.Input>) {
  return (
    <div className="flex items-center gap-3 border-b border-border/70 px-4">
      <Search className="size-5 shrink-0 text-muted-foreground" aria-hidden="true" />
      <CommandPrimitive.Input
        className={cn(
          'h-14 w-full bg-transparent text-lg tracking-[-0.01em] outline-none placeholder:text-muted-foreground',
          className,
        )}
        {...props}
      />
    </div>
  )
}

export function CommandList({ className, ...props }: ComponentProps<typeof CommandPrimitive.List>) {
  return (
    <CommandPrimitive.List
      className={cn('max-h-[50dvh] overflow-x-hidden overflow-y-auto p-2', className)}
      {...props}
    />
  )
}

export function CommandEmpty(props: ComponentProps<typeof CommandPrimitive.Empty>) {
  return (
    <CommandPrimitive.Empty className="py-6 text-center text-sm text-muted-foreground" {...props}>
      {props.children ?? t('shell.palette.empty')}
    </CommandPrimitive.Empty>
  )
}

export function CommandGroup({
  className,
  ...props
}: ComponentProps<typeof CommandPrimitive.Group>) {
  return <CommandPrimitive.Group className={cn('overflow-hidden', className)} {...props} />
}

export function CommandItem({ className, ...props }: ComponentProps<typeof CommandPrimitive.Item>) {
  return (
    <CommandPrimitive.Item
      className={cn(
        'flex min-h-(--control-h) cursor-pointer items-center gap-2.5 rounded-md px-3 py-1.5 text-sm select-none data-[disabled=true]:pointer-events-none data-[disabled=true]:opacity-40 data-[selected=true]:bg-accent data-[selected=true]:text-on-accent [&_svg]:size-4 [&_svg]:shrink-0 [&_svg]:text-muted-foreground data-[selected=true]:**:text-on-accent',
        className,
      )}
      {...props}
    />
  )
}

export function CommandSeparator({
  className,
  ...props
}: ComponentProps<typeof CommandPrimitive.Separator>) {
  return (
    <CommandPrimitive.Separator className={cn('-mx-1 my-1 h-px bg-border', className)} {...props} />
  )
}
