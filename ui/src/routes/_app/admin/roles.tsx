import { createFileRoute } from '@tanstack/react-router'
import { RolesPage } from '@/features/roles/roles-page'
import { pickText } from '@/features/roles/search-params'

export const Route = createFileRoute('/_app/admin/roles')({
  staticData: { crumb: 'nav.items.roles', access: { area: 'roles' } },
  validateSearch: (search: Record<string, unknown>) =>
    pickText(search, ['q', 'builtin', 'edit', 'new'] as const),
  component: RolesPage,
})
