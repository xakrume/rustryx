import type { QueryClient } from '@tanstack/react-query'
import { createRouter, type RouterHistory } from '@tanstack/react-router'
import { NotFoundPage } from '@/features/errors/not-found-page'
import { PagePending } from '@/features/errors/page-pending'
import { RouteErrorPage } from '@/features/errors/route-error-page'
import { routeTree } from '@/routeTree.gen'

export interface AppRouterOptions {
  queryClient: QueryClient
  /** Tests pass a memory history; the app uses the browser history. */
  history?: RouterHistory
}

export function createAppRouter({ queryClient, history }: AppRouterOptions) {
  return createRouter({
    routeTree,
    history,
    context: { queryClient },
    // Prefetch a route's chunk and data when the pointer or focus is on its link.
    defaultPreload: 'intent',
    scrollRestoration: true,
    defaultNotFoundComponent: NotFoundPage,
    defaultErrorComponent: RouteErrorPage,
    defaultPendingComponent: PagePending,
    // Show the pending state only for waits that are noticeable, and keep it up long
    // enough not to flash (same idea as the 150 ms table skeleton).
    defaultPendingMs: 200,
    defaultPendingMinMs: 300,
  })
}

declare module '@tanstack/react-router' {
  interface Register {
    router: ReturnType<typeof createAppRouter>
  }
}
