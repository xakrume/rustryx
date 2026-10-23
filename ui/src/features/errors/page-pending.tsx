import { Skeleton } from '@/components/ui/skeleton'
import { t } from '@/i18n'

/**
 * Shown while a route chunk or loader is pending. Space is reserved (a title bar and a block)
 * so the page does not jump when the content arrives (CLS), and the status is announced.
 */
export function PagePending() {
  return (
    <div role="status" aria-busy="true" className="space-y-4">
      <span className="sr-only">{t('common.loading')}</span>
      <Skeleton className="h-7 w-56" />
      <Skeleton className="h-48 w-full" />
    </div>
  )
}
