import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/admin/roles')({
  staticData: { crumb: 'nav.items.roles', access: { area: 'roles' } },
  component: () => (
    <PlaceholderPage titleKey="nav.items.roles" route="/admin/roles" milestone="M1" />
  ),
})
