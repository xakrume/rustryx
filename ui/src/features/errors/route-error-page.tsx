import { Link, useRouter, type ErrorComponentProps } from '@tanstack/react-router'
import { ErrorPanel } from '@/components/error-panel'
import { Button } from '@/components/ui/button'
import { isApiError } from '@/api/errors'
import { t } from '@/i18n'

/**
 * Router level error boundary (defaultErrorComponent). An API failure that escaped a loader
 * or a suspense query is shown with its code and request id and a Retry; any other thrown
 * value is a bug in the interface and gets the generic "This page crashed" with a way out.
 */
export function RouteErrorPage({ error, reset }: ErrorComponentProps) {
  const router = useRouter()
  const retry = (): void => {
    reset()
    void router.invalidate()
  }
  if (isApiError(error)) {
    return (
      <div className="mx-auto max-w-2xl p-4">
        <h1 data-page-title="" tabIndex={-1} className="sr-only">
          {t('errors.title.unexpected')}
        </h1>
        <ErrorPanel error={error} onRetry={retry} />
      </div>
    )
  }
  return (
    <div className="mx-auto flex max-w-md flex-col items-center gap-3 p-8 text-center">
      <h1 data-page-title="" tabIndex={-1} className="text-xl font-semibold outline-none">
        {t('errors.boundary.title')}
      </h1>
      <p className="text-sm text-muted-foreground">{t('errors.boundary.body')}</p>
      <div className="flex gap-2">
        <Button onClick={retry}>{t('common.retry')}</Button>
        <Button variant="outline" asChild>
          <Link to="/">{t('errors.boundary.home')}</Link>
        </Button>
      </div>
    </div>
  )
}
