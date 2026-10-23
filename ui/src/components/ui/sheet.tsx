import type { ComponentProps } from 'react'
import { cn } from '@/lib/utils'
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from './dialog'

/** A side drawer: the mobile navigation and detail panels. Same behaviour as Dialog. */
export {
  Dialog as Sheet,
  DialogClose as SheetClose,
  DialogDescription as SheetDescription,
  DialogHeader as SheetHeader,
  DialogTitle as SheetTitle,
  DialogTrigger as SheetTrigger,
}

export function SheetContent({
  side = 'left',
  className,
  ...props
}: Omit<ComponentProps<typeof DialogContent>, 'placement'> & { side?: 'left' | 'right' }) {
  return <DialogContent placement={side} className={cn('p-0', className)} {...props} />
}
