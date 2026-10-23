import { Component, type ErrorInfo, type ReactNode } from 'react'
import { Button } from '@/components/ui/button'
import { t } from '@/i18n'

interface Props {
  children: ReactNode
  /** Custom fallback. The default is a small page level message with reload and home. */
  fallback?: (reset: () => void) => ReactNode
}

interface State {
  failed: boolean
}

/**
 * Last resort for render errors outside the router (providers, shell). Route level errors are
 * caught by the router's own boundary and shown by RouteErrorPage. The error itself is not
 * shown to the user (it can hold internals); it goes to the console for the developer.
 */
export class ErrorBoundary extends Component<Props, State> {
  override state: State = { failed: false }

  static getDerivedStateFromError(): State {
    return { failed: true }
  }

  override componentDidCatch(error: Error, info: ErrorInfo): void {
    // The one place that logs: there is nobody else to tell, and swallowing it would hide the bug.
    console.error('Unhandled render error', error, info.componentStack)
  }

  reset = (): void => {
    this.setState({ failed: false })
  }

  override render(): ReactNode {
    if (!this.state.failed) return this.props.children
    if (this.props.fallback) return this.props.fallback(this.reset)
    return (
      <main
        id="main-content"
        tabIndex={-1}
        className="mx-auto flex min-h-dvh max-w-md flex-col items-center justify-center gap-3 p-6 text-center outline-none"
      >
        <h1 data-page-title="" tabIndex={-1} className="text-xl font-semibold outline-none">
          {t('errors.boundary.title')}
        </h1>
        <p className="text-sm text-muted-foreground">{t('errors.boundary.body')}</p>
        <div className="flex gap-2">
          <Button onClick={() => window.location.reload()}>{t('common.reload')}</Button>
          <Button variant="outline" asChild>
            <a href="/">{t('errors.boundary.home')}</a>
          </Button>
        </div>
      </main>
    )
  }
}
