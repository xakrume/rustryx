import { createFileRoute, Outlet } from '@tanstack/react-router'

// Layout route: gives its subtree one breadcrumb and one access rule. Pages below add their own.
export const Route = createFileRoute('/_app/admin/blob-stores')({
  staticData: { crumb: 'nav.items.blobStores', access: { area: 'blobstores' } },
  component: Outlet,
})
