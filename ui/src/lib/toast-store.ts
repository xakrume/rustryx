import { describeError } from '@/lib/describe-error'

export type ToastVariant = 'default' | 'success' | 'info' | 'warning' | 'error'

export interface ToastAction {
  label: string
  onClick: () => void
}

export interface ToastItem {
  id: string
  variant: ToastVariant
  title: string
  description?: string
  /** Stable error code and request id of a failed call, each shown with a copy button. */
  code?: string
  requestId?: string
  action?: ToastAction
  durationMs: number
}

export interface ToastOptions {
  description?: string
  action?: ToastAction
  durationMs?: number
}

/** Errors stay long enough to copy the request id; hover and focus pause the timer anyway. */
const DURATION: Record<ToastVariant, number> = {
  default: 5000,
  success: 5000,
  info: 6000,
  warning: 8000,
  error: 12000,
}

const MAX_VISIBLE = 4

let items: readonly ToastItem[] = []
let counter = 0
const listeners = new Set<() => void>()

function publish(next: readonly ToastItem[]): void {
  items = next
  listeners.forEach((listener) => listener())
}

function add(
  variant: ToastVariant,
  title: string,
  options: ToastOptions & Partial<ToastItem> = {},
): string {
  counter += 1
  const id = `toast-${Date.now()}-${counter}`
  const item: ToastItem = {
    id,
    variant,
    title,
    description: options.description,
    code: options.code,
    requestId: options.requestId,
    action: options.action,
    durationMs: options.durationMs ?? DURATION[variant],
  }
  // Immutable update; the oldest toast makes room when too many are open.
  publish([...items, item].slice(-MAX_VISIBLE))
  return id
}

// Arrow properties, not methods: they are handed to useSyncExternalStore without `this`.
export const toastStore = {
  subscribe: (listener: () => void): (() => void) => {
    listeners.add(listener)
    return () => listeners.delete(listener)
  },
  getSnapshot: (): readonly ToastItem[] => items,
  dismiss: (id: string): void => {
    publish(items.filter((item) => item.id !== id))
  },
  clear: (): void => {
    publish([])
  },
}

/** Fire and forget notifications. Success and failure of every action ends in one of these. */
export const toast = {
  show: (title: string, options?: ToastOptions) => add('default', title, options),
  success: (title: string, options?: ToastOptions) => add('success', title, options),
  info: (title: string, options?: ToastOptions) => add('info', title, options),
  warning: (title: string, options?: ToastOptions) => add('warning', title, options),
  error: (title: string, options?: ToastOptions) => add('error', title, options),
  /**
   * A failed API call: the title says what kind of failure it was, the description is the
   * server's safe message, and the stable `code` and `request_id` come with copy buttons
   * (docs/ui-design-brief.md rule 4). `context` names the action that failed.
   */
  apiError(error: unknown, context?: string): string {
    const d = describeError(error)
    return add('error', context ?? d.title, {
      description: context ? `${d.title}. ${d.message}` : d.message,
      code: d.code ?? undefined,
      requestId: d.requestId ?? undefined,
    })
  },
  dismiss: (id: string) => toastStore.dismiss(id),
}
