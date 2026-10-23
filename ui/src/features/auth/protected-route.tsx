import { useLocation, useMatches, useRouter } from '@tanstack/react-router'
import { useEffect, useRef, type ReactNode } from 'react'
import { ForbiddenPanel } from '@/components/forbidden-panel'
import { describeNeed, satisfies, type PermissionNeed } from './permissions'
import { useMe, usePermissions } from './use-permissions'

export interface ProtectedRouteProps {
  /**
   * Explicit requirement. Without it the requirements come from the `staticData.access` of every
   * matched route (a layout route can protect a whole subtree), which is how the shell guards
   * pages: a page agent only sets `access` in its route file.
   */
  need?: 'public' | PermissionNeed
  children: ReactNode
}

/**
 * Sends the visitor to the login page exactly once. <Navigate> was not used: its effect depends
 * on its props, and a new `search` object on every render made it navigate again while the
 * previous navigation was still pending, which looped until the tab ran out of memory.
 */
function RedirectToLogin({ returnTo }: { returnTo: string }) {
  const router = useRouter()
  const started = useRef(false)
  useEffect(() => {
    if (started.current) return
    started.current = true
    void router.navigate({ to: '/login', search: { return_to: returnTo }, replace: true })
  }, [router, returnTo])
  return null
}

/**
 * Gate driven by `/me`. Unmet requirement and anonymous: send to the login page and come back
 * to the same URL (`return_to`). Unmet requirement and signed in: a page level "You do not
 * have access" naming the privilege, not a blank page. This is navigation comfort: every API
 * call is authorized again by the server, which stays the authority.
 */
export function ProtectedRoute({ need, children }: ProtectedRouteProps) {
  const me = useMe()
  const permissions = usePermissions()
  const matches = useMatches()
  const location = useLocation()

  const rules: (PermissionNeed | 'public')[] =
    need !== undefined
      ? [need]
      : matches.flatMap((m) => (m.staticData.access === undefined ? [] : [m.staticData.access]))
  const unmet = rules.find((rule) => rule !== 'public' && !satisfies(rule, permissions))

  // `/me` is loaded by the shell's beforeLoad; this only covers a cache that was cleared.
  if (me === undefined) return null
  if (unmet === undefined) return <>{children}</>
  if (!permissions.authenticated) {
    return <RedirectToLogin returnTo={location.href} />
  }
  return <ForbiddenPanel privilege={unmet === 'public' ? undefined : describeNeed(unmet)} />
}
