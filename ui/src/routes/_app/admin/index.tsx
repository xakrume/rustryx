import { createFileRoute, redirect } from '@tanstack/react-router'

// /admin has no page of its own: the status page is the natural first stop.
export const Route = createFileRoute('/_app/admin/')({
  beforeLoad: () => {
    throw redirect({ to: '/admin/system/status', replace: true })
  },
})
