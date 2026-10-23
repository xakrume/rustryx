import { createFileRoute, Outlet } from '@tanstack/react-router'

// Layout route: gives its subtree one breadcrumb. Pages below add their own.
export const Route = createFileRoute('/_app/repositories')({
  staticData: { crumb: 'nav.items.repositories' },
  component: Outlet,
})
