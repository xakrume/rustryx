import { useEffect } from 'react'
import { t } from '@/i18n'

/** Keeps document.title in step with the page heading, so tabs, history and screen readers agree. */
export function usePageTitle(title: string | undefined): void {
  useEffect(() => {
    if (!title) return undefined
    const previous = document.title
    document.title = `${title} - ${t('common.appName')}`
    return () => {
      document.title = previous
    }
  }, [title])
}
