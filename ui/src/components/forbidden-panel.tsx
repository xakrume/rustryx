import { ShieldAlert } from 'lucide-react'
import { t } from '@/i18n'
import { cn } from '@/lib/utils'

export interface ForbiddenPanelProps {
  /** Name of the missing privilege, when known (from x-required-privilege or the page rule). */
  privilege?: string
  className?: string
}

/**
 * A page level "You do not have access" (docs/ui-page-inventory.md, section 5). It names the
 * missing privilege and who can grant it, instead of a blank page or a bare 403.
 */
export function ForbiddenPanel({ privilege, className }: ForbiddenPanelProps) {
  return (
    <section
      aria-labelledby="forbidden-title"
      className={cn(
        'mx-auto flex max-w-lg flex-col items-center gap-2 rounded-xl border border-transparent bg-card dark:border-border/60 px-6 py-10 text-center',
        className,
      )}
    >
      <ShieldAlert className="size-8 text-warning-text" aria-hidden="true" />
      <h1
        id="forbidden-title"
        data-page-title=""
        tabIndex={-1}
        className="text-lg font-semibold outline-none"
      >
        {t('errors.forbiddenPage.title')}
      </h1>
      <p className="text-sm text-muted-foreground">{t('errors.forbiddenPage.body')}</p>
      {privilege ? (
        <p className="text-sm">{t('errors.forbiddenPage.missing', { privilege })}</p>
      ) : null}
      <p className="text-sm text-muted-foreground">{t('errors.forbiddenPage.who')}</p>
    </section>
  )
}
