import { createFileRoute } from '@tanstack/react-router'
import { pickText } from '@/features/roles/search-params'
import { AdminTokensPage } from '@/features/tokens/admin-tokens-page'

export const Route = createFileRoute('/_app/admin/tokens')({
  staticData: { crumb: 'nav.items.apiTokens', access: { area: 'tokens' } },
  validateSearch: (search: Record<string, unknown>) =>
    pickText(search, ['state', 'kind', 'never_used', 'expiring', 'owner_id', 'owner'] as const),
  component: AdminTokensPage,
})
