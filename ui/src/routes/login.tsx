import { createFileRoute, redirect } from '@tanstack/react-router'
import { sessionQuery } from '@/features/auth/api'
import { LoginPage, type LoginSearch } from '@/features/auth/login-page'
import { sanitizeReturnTo } from '@/features/auth/return-to'

const text = (value: unknown): string | undefined => (typeof value === 'string' ? value : undefined)

/**
 * `validateSearch` is not code split by the router (it is needed to build links), so it is
 * written by hand instead of with zod: zod would otherwise land in the entry bundle for the
 * sake of four optional strings. A mangled value is dropped, never an error page.
 */
export function validateLoginSearch(search: Record<string, unknown>): LoginSearch {
  return {
    return_to: text(search.return_to),
    error: text(search.error),
    request_id: text(search.request_id),
    reason: search.reason === 'expired' ? 'expired' : undefined,
  }
}

export const Route = createFileRoute('/login')({
  validateSearch: validateLoginSearch,
  beforeLoad: async ({ context, search }) => {
    // Already signed in (another tab, a bookmark): go where the user was heading.
    // A failing session call (server down) must not block the page that explains it.
    // Not when a 401 sent the user here (`reason=expired`): the server just said this session
    // does not work, so bouncing back would loop between the page and the login page.
    const session = await context.queryClient.ensureQueryData(sessionQuery).catch(() => null)
    if (session?.authenticated === true && search.reason !== 'expired') {
      throw redirect({ href: sanitizeReturnTo(search.return_to), replace: true })
    }
  },
  component: function LoginRoute() {
    return <LoginPage search={Route.useSearch()} />
  },
})
