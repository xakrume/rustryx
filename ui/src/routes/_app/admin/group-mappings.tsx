import { createFileRoute } from '@tanstack/react-router'
import { MappingsPage } from '@/features/idp-mappings/mappings-page'
import { pickText } from '@/features/roles/search-params'

export const Route = createFileRoute('/_app/admin/group-mappings')({
  staticData: { crumb: 'nav.items.groupMappings', access: { area: 'mappings' } },
  validateSearch: (search: Record<string, unknown>) =>
    pickText(search, ['provider', 'edit', 'new'] as const),
  component: MappingsPage,
})
