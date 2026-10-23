import { createFileRoute, Outlet } from '@tanstack/react-router'

// Layout route: gives its subtree one breadcrumb. Pages below add their own.
export const Route = createFileRoute('/_app/admin')({
  staticData: { crumb: 'pages.administration' },
  component: Outlet,
})
