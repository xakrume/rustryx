import { createFileRoute } from '@tanstack/react-router'
import { meQuery, sessionQuery } from '@/features/auth/api'
import { AppShell } from '@/features/shell/app-shell'

/**
 * Pathless layout of every page that lives in the shell. Both bootstrap calls answer 200 for
 * an anonymous visitor, so they never redirect: pages decide who may enter (staticData.access,
 * enforced by ProtectedRoute inside the shell). A failure here is an outage, and the router
 * error page shows it with its request id and a Retry.
 */
export const Route = createFileRoute('/_app')({
  beforeLoad: async ({ context }) => {
    await Promise.all([
      context.queryClient.ensureQueryData(sessionQuery),
      context.queryClient.ensureQueryData(meQuery),
    ])
  },
  component: AppShell,
})
