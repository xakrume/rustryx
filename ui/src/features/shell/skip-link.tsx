import { t } from '@/i18n'

/**
 * First focusable element of every page: lets keyboard users jump over the navigation.
 * Hidden until focused. It moves focus programmatically instead of relying on the hash, so
 * the router does not treat it as a navigation.
 */
export function SkipLink() {
  return (
    <a
      href="#main-content"
      onClick={(event) => {
        event.preventDefault()
        document.getElementById('main-content')?.focus()
      }}
      className="sr-only focus:not-sr-only focus:fixed focus:top-2 focus:left-2 focus:z-[200] focus:rounded-md focus:bg-accent focus:px-3 focus:py-2 focus:text-sm focus:font-medium focus:text-on-accent"
    >
      {t('a11y.skipToContent')}
    </a>
  )
}
