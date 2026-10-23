import { useQueryClient } from '@tanstack/react-query'
import { useRouter } from '@tanstack/react-router'
import { useEffect } from 'react'
import { apiEvents } from '@/api/events'
import { setCsrfToken } from '@/api/csrf'
import { sanitizeReturnTo } from './return-to'

/**
 * A 401 on a cookie session means it ended (idle or absolute timeout, revoked, signed out
 * elsewhere): drop everything cached for the old principal and go to the login page, with
 * `return_to` set to where the user was (docs/api-conventions.md, 5.3). Many requests fail
 * together when a session ends, so only the first event acts.
 */
export function AuthRedirector() {
  const router = useRouter()
  const queryClient = useQueryClient()

  useEffect(() => {
    let redirecting = false
    const unsubscribe = apiEvents.subscribe((event) => {
      if (event.type !== 'unauthenticated' || redirecting) return
      const here = router.state.location
      if (here.pathname === '/login') return
      redirecting = true
      setCsrfToken(null)
      queryClient.clear()
      void router
        .navigate({
          to: '/login',
          search: { return_to: sanitizeReturnTo(here.href), reason: 'expired' },
          replace: true,
        })
        .finally(() => {
          redirecting = false
        })
    })
    return unsubscribe
  }, [router, queryClient])

  return null
}
