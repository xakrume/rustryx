import { createRootRouteWithContext, Outlet } from '@tanstack/react-router'
import type { RouterContext } from '@/app/router-context'
import { AuthRedirector } from '@/features/auth/auth-redirector'
import { RouteFocusManager } from '@/features/shell/route-focus-manager'
import { SkipLink } from '@/features/shell/skip-link'

export const Route = createRootRouteWithContext<RouterContext>()({
  component: () => (
    <>
      <SkipLink />
      <Outlet />
      <AuthRedirector />
      <RouteFocusManager />
    </>
  ),
})
