import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/admin/group-mappings')({
  staticData: { crumb: 'nav.items.groupMappings', access: { area: 'mappings' } },
  component: () => (
    <PlaceholderPage
      titleKey="nav.items.groupMappings"
      route="/admin/group-mappings"
      milestone="M1"
    />
  ),
})
