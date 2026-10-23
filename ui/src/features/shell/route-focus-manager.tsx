import { useRouter } from '@tanstack/react-router'
import { useEffect, useState } from 'react'
import { t } from '@/i18n'

const HEADING_WAIT_MS = 3000

function waitForHeading(main: HTMLElement): void {
  const observer = new MutationObserver(() => {
    const heading = document.querySelector<HTMLElement>('[data-page-title]')
    if (!heading) return
    observer.disconnect()
    const parked = document.activeElement === main || document.activeElement === document.body
    if (parked) heading.focus({ preventScroll: true })
  })
  observer.observe(main, { childList: true, subtree: true })
  setTimeout(() => observer.disconnect(), HEADING_WAIT_MS)
}

/**
 * Focus management after navigation (WCAG 2.4.3). A single page app does not reload, so
 * without this a keyboard user keeps focus on the link that was just activated, which is now
 * detached or far from the new content, and a screen reader says nothing. After every route
 * change that alters the pathname (not a filter or sort edit in the query string, which must
 * not steal focus from the field being typed in) focus moves to the page's <h1>
 * (data-page-title, set by PageHeader), else to <main>, and the new title is announced.
 */
export function RouteFocusManager() {
  const router = useRouter()
  const [announcement, setAnnouncement] = useState('')

  useEffect(() => {
    return router.subscribe('onResolved', (event) => {
      // The first resolve is the initial page load: leave focus where the browser put it.
      if (!event.pathChanged || event.fromLocation === undefined) return
      requestAnimationFrame(() => {
        const main = document.getElementById('main-content')
        const heading = document.querySelector<HTMLElement>('[data-page-title]')
        if (heading) {
          heading.focus({ preventScroll: true })
        } else {
          // The page body is still loading (a lazily imported component): do not leave focus on a
          // link that is gone. Park it on <main>, and hand it to the heading when that appears,
          // unless the user already moved on.
          main?.focus({ preventScroll: true })
          if (main) waitForHeading(main)
        }
        const title = document.title.replace(/ - [^-]*$/, '')
        setAnnouncement(t('a11y.navigatedTo', { title }))
      })
    })
  }, [router])

  return (
    <p role="status" aria-live="polite" className="sr-only">
      {announcement}
    </p>
  )
}
