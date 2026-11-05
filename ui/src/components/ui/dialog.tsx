import { DismissableLayer } from '@radix-ui/react-dismissable-layer'
import {
  FocusScope,
  FocusScopeBranchProvider,
  useFocusScopeBranchRegistry,
} from '@radix-ui/react-focus-scope'
import { Portal } from '@radix-ui/react-portal'
import { Presence } from '@radix-ui/react-presence'
import { Slot } from '@radix-ui/react-slot'
import { hideOthers } from 'aria-hidden'
import { X } from 'lucide-react'
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type ComponentProps,
  type ReactNode,
  type RefObject,
} from 'react'
import { t } from '@/i18n'
import { cn } from '@/lib/utils'

/**
 * Modal dialog built from the Radix focus, dismiss, portal and presence primitives instead of
 * @radix-ui/react-dialog.
 *
 * Why not the Radix Dialog: it locks page scroll with react-remove-scroll, which appends a
 * <style> element at runtime. The production Content-Security-Policy is `style-src 'self'`
 * (see scripts/csp.mjs), which blocks that element, so every dialog would raise a CSP
 * violation and scroll locking would silently not work. Here the lock is plain CSS
 * (`body:has([data-dialog-open])`, styles/index.css) and the app shell scrolls inside <main>,
 * so the page behind a dialog has nothing to scroll anyway.
 *
 * Kept from Radix: focus trap and restore, Escape and outside click dismissal, aria-hidden on
 * everything else while open, nested layer (menu, popover) support through focus scope branches.
 */
interface DialogContextValue {
  open: boolean
  setOpen: (open: boolean) => void
  titleId: string
  descriptionId: string
  contentId: string
}

const DialogContext = createContext<DialogContextValue | null>(null)

function useDialogContext(part: string): DialogContextValue {
  const ctx = useContext(DialogContext)
  if (ctx === null) throw new Error(`${part} must be rendered inside <Dialog>`)
  return ctx
}

export interface DialogProps {
  open?: boolean
  defaultOpen?: boolean
  onOpenChange?: (open: boolean) => void
  children: ReactNode
}

export function Dialog({
  open: openProp,
  defaultOpen = false,
  onOpenChange,
  children,
}: DialogProps) {
  const [uncontrolled, setUncontrolled] = useState(defaultOpen)
  const controlled = openProp !== undefined
  const open = controlled ? openProp : uncontrolled
  const setOpen = useCallback(
    (next: boolean) => {
      if (!controlled) setUncontrolled(next)
      onOpenChange?.(next)
    },
    [controlled, onOpenChange],
  )
  const id = useId()
  const value = useMemo<DialogContextValue>(
    () => ({
      open,
      setOpen,
      titleId: `${id}-title`,
      descriptionId: `${id}-description`,
      contentId: `${id}-content`,
    }),
    [open, setOpen, id],
  )
  return <DialogContext.Provider value={value}>{children}</DialogContext.Provider>
}

export function DialogTrigger({
  asChild,
  ...props
}: ComponentProps<'button'> & { asChild?: boolean }) {
  const ctx = useDialogContext('DialogTrigger')
  const Comp = asChild ? Slot : 'button'
  return (
    <Comp
      type={asChild ? undefined : 'button'}
      aria-haspopup="dialog"
      aria-expanded={ctx.open}
      aria-controls={ctx.open ? ctx.contentId : undefined}
      onClick={() => ctx.setOpen(!ctx.open)}
      {...props}
    />
  )
}

export function DialogClose({
  asChild,
  onClick,
  ...props
}: ComponentProps<'button'> & { asChild?: boolean }) {
  const ctx = useDialogContext('DialogClose')
  const Comp = asChild ? Slot : 'button'
  return (
    <Comp
      type={asChild ? undefined : 'button'}
      onClick={(event: React.MouseEvent<HTMLButtonElement>) => {
        onClick?.(event)
        if (!event.defaultPrevented) ctx.setOpen(false)
      }}
      {...props}
    />
  )
}

type Placement = 'center' | 'left' | 'right'

const placementClasses: Record<Placement, string> = {
  center:
    'left-1/2 top-1/2 max-h-[calc(100dvh-2rem)] w-[calc(100%-2rem)] max-w-lg -translate-x-1/2 -translate-y-1/2 rounded-2xl border border-border/60',
  left: 'inset-y-0 left-0 h-dvh w-72 max-w-[85vw] border-r border-border data-[state=open]:animate-slide-in-left data-[state=closed]:animate-slide-out-left',
  right: 'inset-y-0 right-0 h-dvh w-96 max-w-[90vw] border-l border-border',
}

export interface DialogContentProps extends Omit<ComponentProps<'div'>, 'role' | 'ref'> {
  placement?: Placement
  role?: 'dialog' | 'alertdialog'
  /** False keeps the dialog open on Escape and outside clicks (secret dialogs before acknowledgement). */
  dismissible?: boolean
  /** Hide the corner close button. Use when the footer already offers every way out. */
  hideCloseButton?: boolean
  /** An element with this attribute inside the content receives the initial focus. */
  onOpenAutoFocus?: (event: Event) => void
}

/**
 * Initial focus goes to the first element marked `data-autofocus` (a confirm dialog marks
 * Cancel, the safe choice), else to the first tabbable element, which is Radix's default.
 */
function focusMarked(content: HTMLElement | null, event: Event): void {
  const marked = content?.querySelector<HTMLElement>('[data-autofocus]')
  if (marked) {
    event.preventDefault()
    marked.focus()
  }
}

/**
 * Marks everything outside the dialog aria-hidden while it is mounted. It is a child of the
 * content on purpose: the content only exists after the portal mounted (one render later than
 * <DialogContent>), so an effect in DialogContent itself would run before there is a node.
 */
function HideOthers({ target }: { target: RefObject<HTMLElement | null> }) {
  useEffect(() => {
    const node = target.current
    if (node === null) return undefined
    return hideOthers(node)
  }, [target])
  return null
}

export function DialogContent({
  className,
  children,
  placement = 'center',
  role = 'dialog',
  dismissible = true,
  hideCloseButton = false,
  onOpenAutoFocus,
  ...props
}: DialogContentProps) {
  const ctx = useDialogContext('DialogContent')
  const contentRef = useRef<HTMLDivElement | null>(null)
  const { nodes, registry } = useFocusScopeBranchRegistry()
  const state = ctx.open ? 'open' : 'closed'

  return (
    <>
      <Presence present={ctx.open}>
        <Portal asChild>
          <div
            aria-hidden="true"
            data-state={state}
            // Radix switches pointer events off on <body> while a modal layer is open; the
            // overlay must stay clickable so a click on it dismisses the dialog.
            style={{ pointerEvents: 'auto' }}
            className="fixed inset-0 z-50 bg-overlay backdrop-blur-sm data-[state=open]:animate-fade-in data-[state=closed]:animate-fade-out"
          />
        </Portal>
      </Presence>
      <Presence present={ctx.open}>
        <Portal asChild>
          <FocusScopeBranchProvider registry={registry}>
            <FocusScope
              asChild
              loop
              trapped={ctx.open}
              branches={nodes}
              onMountAutoFocus={(event) => {
                onOpenAutoFocus?.(event)
                if (!event.defaultPrevented) focusMarked(contentRef.current, event)
              }}
            >
              <DismissableLayer
                ref={contentRef}
                role={role}
                id={ctx.contentId}
                aria-labelledby={ctx.titleId}
                aria-describedby={ctx.descriptionId}
                data-state={state}
                data-dialog-open=""
                disableOutsidePointerEvents={ctx.open}
                onEscapeKeyDown={(event) => {
                  if (!dismissible) event.preventDefault()
                }}
                onPointerDownOutside={(event) => {
                  if (!dismissible) event.preventDefault()
                }}
                // Focus may legitimately move to portalled nested layers: never dismiss on it.
                onFocusOutside={(event) => event.preventDefault()}
                onDismiss={() => ctx.setOpen(false)}
                className={cn(
                  'fixed z-50 flex flex-col overflow-y-auto bg-popover p-6 text-popover-foreground shadow-xl data-[state=open]:animate-fade-in data-[state=closed]:animate-fade-out',
                  placementClasses[placement],
                  className,
                )}
                {...props}
              >
                <HideOthers target={contentRef} />
                {children}
                {dismissible && !hideCloseButton ? (
                  <DialogClose
                    aria-label={t('common.close')}
                    className="absolute top-3 right-3 inline-flex size-(--control-h) items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"
                  >
                    <X className="size-4" aria-hidden="true" />
                  </DialogClose>
                ) : null}
              </DismissableLayer>
            </FocusScope>
          </FocusScopeBranchProvider>
        </Portal>
      </Presence>
    </>
  )
}

export function DialogHeader({ className, ...props }: ComponentProps<'div'>) {
  return <div className={cn('mb-3 flex flex-col gap-1 pr-10', className)} {...props} />
}

export function DialogFooter({ className, ...props }: ComponentProps<'div'>) {
  return (
    <div
      className={cn('mt-6 flex flex-col-reverse gap-2 sm:flex-row sm:justify-end', className)}
      {...props}
    />
  )
}

export function DialogTitle({ className, ...props }: ComponentProps<'h2'>) {
  const ctx = useDialogContext('DialogTitle')
  return (
    // eslint-disable-next-line jsx-a11y/heading-has-content -- the content is the children in props
    <h2
      id={ctx.titleId}
      className={cn(
        'text-xl leading-7 font-semibold tracking-[-0.02em] text-foreground',
        className,
      )}
      {...props}
    />
  )
}

export function DialogDescription({ className, ...props }: ComponentProps<'p'>) {
  const ctx = useDialogContext('DialogDescription')
  return (
    <p
      id={ctx.descriptionId}
      className={cn('text-sm text-muted-foreground', className)}
      {...props}
    />
  )
}
