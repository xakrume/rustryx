import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/admin/tokens')({
  staticData: { crumb: 'nav.items.apiTokens', access: { area: 'tokens' } },
  component: () => (
    <PlaceholderPage titleKey="nav.items.apiTokens" route="/admin/tokens" milestone="M1" />
  ),
})
