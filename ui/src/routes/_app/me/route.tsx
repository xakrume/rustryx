import { createFileRoute, Outlet } from '@tanstack/react-router'

// Layout route: gives its subtree one breadcrumb and one access rule. Pages below add their own.
export const Route = createFileRoute('/_app/me')({
  staticData: { crumb: 'pages.account', access: 'authenticated' },
  component: Outlet,
})
