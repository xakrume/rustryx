import { Link } from '@tanstack/react-router'
import { FileQuestion } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { t } from '@/i18n'
import { usePageTitle } from '@/hooks/use-page-title'

/** 404: no route matches. For a missing OBJECT (a repository) pages use ErrorPanel instead. */
export function NotFoundPage() {
  usePageTitle(t('errors.notFoundPage.title'))
  return (
    <main
      id="main-content"
      tabIndex={-1}
      className="mx-auto flex min-h-[60dvh] max-w-md flex-col items-center justify-center gap-3 p-6 text-center outline-none"
    >
      <FileQuestion className="size-10 text-muted-foreground" aria-hidden="true" />
      <h1 data-page-title="" tabIndex={-1} className="text-xl font-semibold outline-none">
        {t('errors.notFoundPage.title')}
      </h1>
      <p className="text-sm text-muted-foreground">{t('errors.notFoundPage.body')}</p>
      <Button asChild>
        <Link to="/">{t('errors.notFoundPage.home')}</Link>
      </Button>
    </main>
  )
}
