import { createFileRoute } from '@tanstack/react-router'
import { PrivilegesPage } from '@/features/privileges/privileges-page'
import { pickText } from '@/features/roles/search-params'

export const Route = createFileRoute('/_app/admin/privileges')({
  staticData: { crumb: 'nav.items.privileges', access: { area: 'privileges' } },
  validateSearch: (search: Record<string, unknown>) =>
    pickText(search, ['q', 'kind', 'repository', 'builtin', 'edit', 'new'] as const),
  component: PrivilegesPage,
})
