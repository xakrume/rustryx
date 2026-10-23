import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/me/tokens')({
  staticData: { crumb: 'nav.items.personalTokens' },
  component: () => (
    <PlaceholderPage titleKey="nav.items.personalTokens" route="/me/tokens" milestone="M1" />
  ),
})
