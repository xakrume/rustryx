import { AlertTriangle, CheckCircle2, Info, XCircle } from 'lucide-react'
import { useSyncExternalStore } from 'react'
import { CopyButton } from '@/components/copy-button'
import {
  ToastAction,
  ToastClose,
  ToastDescription,
  ToastProvider,
  ToastRoot,
  ToastTitle,
  ToastViewport,
} from '@/components/ui/toast'
import { t } from '@/i18n'
import { toastStore, type ToastItem, type ToastVariant } from '@/lib/toast-store'

const variantStyle: Record<ToastVariant, { border: string; icon: typeof Info; iconClass: string }> =
  {
    default: { border: 'border-l-input', icon: Info, iconClass: 'text-muted-foreground' },
    info: { border: 'border-l-info', icon: Info, iconClass: 'text-info-text' },
    success: { border: 'border-l-success', icon: CheckCircle2, iconClass: 'text-success-text' },
    warning: { border: 'border-l-warning', icon: AlertTriangle, iconClass: 'text-warning-text' },
    error: { border: 'border-l-destructive', icon: XCircle, iconClass: 'text-destructive-text' },
  }

function ToastEntry({ item }: { item: ToastItem }) {
  const style = variantStyle[item.variant]
  const Icon = style.icon
  return (
    <ToastRoot
      // Errors interrupt (assertive live region), everything else waits its turn.
      type={item.variant === 'error' ? 'foreground' : 'background'}
      duration={item.durationMs}
      className={style.border}
      onOpenChange={(open) => {
        if (!open) toastStore.dismiss(item.id)
      }}
    >
      <Icon className={`mt-0.5 size-4 shrink-0 ${style.iconClass}`} aria-hidden="true" />
      <div className="min-w-0 flex-1 space-y-1">
        <ToastTitle className="font-medium">{item.title}</ToastTitle>
        {item.description ? (
          <ToastDescription className="text-muted-foreground">{item.description}</ToastDescription>
        ) : null}
        {item.code || item.requestId ? (
          <ul className="space-y-0.5 text-xs text-muted-foreground">
            {item.code ? (
              <li className="flex items-center gap-1">
                <span>{t('common.errorCode')}</span>
                <code className="font-mono text-foreground">{item.code}</code>
                <CopyButton
                  value={item.code}
                  what={t('common.errorCode')}
                  size="icon"
                  className="size-6"
                  silent
                />
              </li>
            ) : null}
            {item.requestId ? (
              <li className="flex items-center gap-1">
                <span>{t('common.requestId')}</span>
                <code className="font-mono break-all text-foreground">{item.requestId}</code>
                <CopyButton
                  value={item.requestId}
                  what={t('common.requestId')}
                  size="icon"
                  className="size-6"
                  silent
                />
              </li>
            ) : null}
          </ul>
        ) : null}
        {item.action ? (
          <ToastAction altText={item.action.label} asChild>
            <button
              type="button"
              className="mt-1 text-sm font-medium text-link underline underline-offset-2"
              onClick={item.action.onClick}
            >
              {item.action.label}
            </button>
          </ToastAction>
        ) : null}
      </div>
      <ToastClose />
    </ToastRoot>
  )
}

/** Mount once, near the root. Everything else calls `toast.*` from lib/toast-store. */
export function Toaster() {
  const items = useSyncExternalStore(
    toastStore.subscribe,
    toastStore.getSnapshot,
    toastStore.getSnapshot,
  )
  return (
    <ToastProvider swipeDirection="right" duration={5000}>
      {items.map((item) => (
        <ToastEntry key={item.id} item={item} />
      ))}
      <ToastViewport />
    </ToastProvider>
  )
}
