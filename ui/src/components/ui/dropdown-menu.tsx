import * as MenuPrimitive from '@radix-ui/react-dropdown-menu'
import { Check } from 'lucide-react'
import type { ComponentProps } from 'react'
import { cn } from '@/lib/utils'

/**
 * Menus are rendered non-modal. A modal Radix menu runs react-remove-scroll, whose runtime
 * <style> element the strict CSP blocks (see components/ui/dialog.tsx). Non-modal keeps
 * keyboard navigation, typeahead, Escape and focus return; it only skips the scroll lock and
 * the aria-hidden of the rest of the page, which a transient menu does not need.
 */
export function DropdownMenu({
  modal = false,
  ...props
}: ComponentProps<typeof MenuPrimitive.Root>) {
  return <MenuPrimitive.Root modal={modal} {...props} />
}

export const DropdownMenuTrigger = MenuPrimitive.Trigger
export const DropdownMenuGroup = MenuPrimitive.Group
export const DropdownMenuRadioGroup = MenuPrimitive.RadioGroup

const itemBase =
  'relative flex min-h-(--control-h) cursor-pointer items-center gap-2 rounded-sm px-2.5 py-1.5 text-sm outline-none select-none data-[disabled]:pointer-events-none data-[disabled]:opacity-40 data-[highlighted]:bg-accent data-[highlighted]:text-on-accent data-[highlighted]:**:text-on-accent [&_svg]:size-4 [&_svg]:shrink-0'

export function DropdownMenuContent({
  className,
  sideOffset = 6,
  align = 'end',
  ...props
}: ComponentProps<typeof MenuPrimitive.Content>) {
  return (
    <MenuPrimitive.Portal>
      <MenuPrimitive.Content
        sideOffset={sideOffset}
        align={align}
        className={cn(
          'z-50 min-w-52 overflow-y-auto rounded-lg border border-border/60 bg-popover/95 p-1.5 text-popover-foreground shadow-lg backdrop-blur-xl data-[state=closed]:animate-fade-out data-[state=open]:animate-fade-in',
          className,
        )}
        {...props}
      />
    </MenuPrimitive.Portal>
  )
}

export function DropdownMenuItem({
  className,
  ...props
}: ComponentProps<typeof MenuPrimitive.Item>) {
  return <MenuPrimitive.Item className={cn(itemBase, className)} {...props} />
}

export function DropdownMenuCheckboxItem({
  className,
  children,
  ...props
}: ComponentProps<typeof MenuPrimitive.CheckboxItem>) {
  return (
    <MenuPrimitive.CheckboxItem className={cn(itemBase, 'pl-8', className)} {...props}>
      <span className="absolute left-2 flex size-4 items-center justify-center">
        <MenuPrimitive.ItemIndicator>
          <Check className="size-4" aria-hidden="true" />
        </MenuPrimitive.ItemIndicator>
      </span>
      {children}
    </MenuPrimitive.CheckboxItem>
  )
}

export function DropdownMenuRadioItem({
  className,
  children,
  ...props
}: ComponentProps<typeof MenuPrimitive.RadioItem>) {
  return (
    <MenuPrimitive.RadioItem className={cn(itemBase, 'pl-8', className)} {...props}>
      <span className="absolute left-2 flex size-4 items-center justify-center">
        <MenuPrimitive.ItemIndicator>
          <Check className="size-4" aria-hidden="true" />
        </MenuPrimitive.ItemIndicator>
      </span>
      {children}
    </MenuPrimitive.RadioItem>
  )
}

export function DropdownMenuLabel({
  className,
  ...props
}: ComponentProps<typeof MenuPrimitive.Label>) {
  return (
    <MenuPrimitive.Label
      className={cn('px-2 py-1.5 text-xs font-medium text-muted-foreground', className)}
      {...props}
    />
  )
}

export function DropdownMenuSeparator({
  className,
  ...props
}: ComponentProps<typeof MenuPrimitive.Separator>) {
  return (
    <MenuPrimitive.Separator
      className={cn('-mx-1.5 my-1.5 h-px bg-border/70', className)}
      {...props}
    />
  )
}
