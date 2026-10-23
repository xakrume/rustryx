import { createFileRoute, Outlet } from '@tanstack/react-router'

// Layout route: gives its subtree one breadcrumb and one access rule. Pages below add their own.
export const Route = createFileRoute('/_app/admin/users')({
  staticData: { crumb: 'nav.items.users', access: { area: 'users' } },
  component: Outlet,
})
